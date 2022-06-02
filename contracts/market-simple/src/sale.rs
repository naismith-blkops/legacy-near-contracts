use crate::*;
use near_sdk::promise_result_as_success;

//stores bidding information such as the owner and price of the current bid
#[derive(BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Bid {
    pub owner_id: AccountId,
    pub price: U128,
}

//struct that holds important information about each sale on the market
#[derive(BorshDeserialize, BorshSerialize, Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Sale {
    //owner of the sale
    pub owner_id: AccountId,
    //market contract's approval ID to transfer the token on behalf of the owner
    pub approval_id: u64,
    //nft contract where the token was minted
    pub nft_contract_id: AccountId,
    //actual token ID for sale
    pub token_id: TokenId,
    //sale conditions specified when the token was put for sale
    pub sale_conditions: SaleConditions,
    //active bids on the token
    pub bids: Bids,
    //when the sale was created
    pub created_at: U64,
    //is the token an auction
    pub is_auction: bool,
    //what is the type of sale
    pub token_type: Option<String>,
}

#[near_bindgen]
impl Contract {
    //removes a sale from the market. 
    #[payable]
    pub fn remove_sale(&mut self, nft_contract_id: AccountId, token_id: TokenId) {
        //assert that the user has attached exactly 1 yoctoNEAR (for security reasons)
        assert_one_yocto();
        //get the sale object as the return value from removing the sale internally
        let sale = self.internal_remove_sale(nft_contract_id.into(), token_id);
        //get the predecessor of the call and make sure they're the owner of the sale
        let owner_id = env::predecessor_account_id();
        //if this fails, the remove sale will revert
        assert_eq!(owner_id, sale.owner_id, "Must be sale owner");
        //Refund all bids
        self.refund_all_bids(&sale.bids);
    }

    //updates the price for a sale on the market for the given FT.
    #[payable]
    pub fn update_price(
        &mut self,
        nft_contract_id: AccountId,
        token_id: TokenId,
        ft_token_id: AccountId,
        price: U128,
    ) {
        //assert that the user has attached exactly 1 yoctoNEAR (for security reasons)
        assert_one_yocto();
        
        //create the unique sale ID from the nft contract and token
        let contract_id: AccountId = nft_contract_id.into();
        let contract_and_token_id = format!("{}{}{}", contract_id, DELIMETER, token_id);
        
        //get the sale object from the unique sale ID. If there is no token, panic. 
        let mut sale = self.sales.get(&contract_and_token_id).expect("No sale");

        //assert that the caller of the function is the sale owner
        assert_eq!(
            env::predecessor_account_id(),
            sale.owner_id,
            "Must be sale owner"
        );
        //ensure that the market supports the FT
        if !self.ft_token_ids.contains(&ft_token_id) {
            env::panic_str(&format!("Token {} not supported by this market", ft_token_id));
        }

        //update the sale conditions
        sale.sale_conditions.insert(ft_token_id.into(), price);
        //insert the sale back into the map for the unique sale ID
        self.sales.insert(&contract_and_token_id, &sale);
    }

    //place an offer on a specific sale. The sale will go through as long as your deposit is greater than or equal to the list price
    #[payable]
    pub fn offer(&mut self, nft_contract_id: AccountId, token_id: TokenId) {
        //get the attached deposit and make sure it's greater than 0
        let deposit = env::attached_deposit();
        assert!(deposit > 0, "Attached deposit must be greater than 0");

        //get the unique sale ID (contract + DELIMITER + token ID)
        let contract_and_token_id = format!("{}{}{}", nft_contract_id, DELIMETER, token_id);
        
        //get the sale object from the unique sale ID. If the sale doesn't exist, panic.
        let mut sale = self.sales.get(&contract_and_token_id).expect("No sale");
        
        //get the buyer ID which is the person who called the function and make sure they're not the owner of the sale
        let buyer_id = env::predecessor_account_id();
        assert_ne!(sale.owner_id, buyer_id, "Cannot bid on your own sale.");

        //Ensure that the token is for sale for $NEAR
        let ft_token_id: AccountId = "near".parse().unwrap();
        let price = sale
            .sale_conditions
            .get(&ft_token_id)
            .expect("Not for sale in NEAR")
            .0;

        //Process the purchase if no auction and deposit larger or equal to the price.
        if !sale.is_auction && deposit >= price {
            //process the purchase (which will remove the sale, transfer and get the payout from the nft contract, and then distribute royalties) 
            self.process_purchase(
                nft_contract_id,
                token_id,
                ft_token_id,
                U128(deposit),
                buyer_id,
            );
        //Either the NFT is an auction or the deposit is less than the price
         } else {
            //If it's an auction, make sure the deposit is larger than or equal to the reserve price
            if sale.is_auction {
                assert!(deposit >= price, "Attached deposit must be greater than reserve price");
            }
            //If it's not an auction, the deposit must be less than the price so add a bid
            self.add_bid(
                contract_and_token_id,
                deposit,
                ft_token_id,
                buyer_id,
                &mut sale,
            );
        }
    }

    /// Private function for adding bids
    #[private]
    pub fn add_bid(
        &mut self,
        contract_and_token_id: ContractAndTokenId,
        amount: Balance,
        ft_token_id: AccountId,
        buyer_id: AccountId,
        sale: &mut Sale,
    ) {
        // store a bid and refund any current bid lower
        let new_bid = Bid {
            owner_id: buyer_id,
            price: U128(amount),
        };
        
        // get the current bids for the token ID
        let bids_for_token_id = sale.bids.entry(ft_token_id.clone()).or_insert_with(Vec::new);
        
        // if the bids aren't empty, make sure the new bid is larger in price than the current
        if !bids_for_token_id.is_empty() {
            // get the current bid
            let current_bid = &bids_for_token_id[bids_for_token_id.len()-1];
            // panic if the new bid isn't larger in price than the old one. This will refund incoming bidder since everything is on the same receipt. no callback needed.
            assert!(
                amount > current_bid.price.0,
                "Can't pay less than or equal to current bid price: {}",
                current_bid.price.0
            );
            //refund the current bidder since he's been outbid.
            if ft_token_id == "near".parse().unwrap() {
                Promise::new(current_bid.owner_id.clone()).transfer(u128::from(current_bid.price));
            } else {
                ext_contract::ft_transfer(
                    current_bid.owner_id.clone(),
                    current_bid.price,
                    None,
                    ft_token_id,
                    1,
                    GAS_FOR_FT_TRANSFER,
                );
            }
        }
        
        //push the new bid and remove the back of the vector if the max history length has been reached
        bids_for_token_id.push(new_bid);
        if bids_for_token_id.len() > self.bid_history_length as usize {
            bids_for_token_id.remove(0);
        }
        
        //insert the new sale object back into the map
        self.sales.insert(&contract_and_token_id, &sale);
    }

    /// Accept an offer (only token owner can call this)
    pub fn accept_offer(
        &mut self,
        nft_contract_id: AccountId,
        token_id: TokenId,
        ft_token_id: AccountId,
    ) {
        // Get the sale object
        let contract_and_token_id = format!("{}{}{}", nft_contract_id, DELIMETER, token_id.clone());
        // remove bid before proceeding to process purchase
        let mut sale = self.sales.get(&contract_and_token_id).expect("No sale");
        let bids_for_token_id = sale.bids.remove(&ft_token_id).expect("No bids");
        // get the current bid
        let bid = &bids_for_token_id[bids_for_token_id.len()-1];
        self.sales.insert(&contract_and_token_id, &sale);
        // panics at `self.internal_remove_sale` and reverts above if predecessor is not sale.owner_id
        self.process_purchase(
            nft_contract_id,
            token_id,
            ft_token_id.into(),
            bid.price,
            bid.owner_id.clone(),
        );
    }

    //private function used when a sale is purchased. 
    //this will remove the sale, transfer and get the payout from the nft contract, and then distribute royalties
    #[private]
    pub fn process_purchase(
        &mut self,
        nft_contract_id: AccountId,
        token_id: TokenId,
        ft_token_id: AccountId,
        price: U128,
        buyer_id: AccountId,
    ) -> Promise {
        //get the sale object by removing the sale
        let sale = self.internal_remove_sale(nft_contract_id.clone(), token_id.clone());

        //initiate a cross contract call to the nft contract. This will transfer the token to the buyer and return
        //a payout object used for the market to distribute funds to the appropriate accounts.
        ext_contract::nft_transfer_payout(
            buyer_id.clone(), //purchaser (person to transfer the NFT to)
            token_id, //token ID to transfer
            sale.approval_id, //market contract's approval ID in order to transfer the token on behalf of the owner
            "payout from market".to_string(), //memo (to include some context)
            /*
                the price that the token was purchased for. This will be used in conjunction with the royalty percentages
                for the token in order to determine how much money should go to which account. 
            */
            price,
			10, //the maximum amount of accounts the market can payout at once (this is limited by GAS)
            nft_contract_id, //contract to initiate the cross contract call to
            1, //yoctoNEAR to attach to the call
            GAS_FOR_NFT_TRANSFER, //GAS to attach to the call
        )
        //after the transfer payout has been finalized, we resolve the promise by calling our own resolve_purchase function. 
        //resolve purchase will take the payout object returned from the nft_transfer_payout and actually pay the accounts (if it was purchased for $NEAR)
        .then(ext_self::resolve_purchase(
            ft_token_id, // what FT it was purchased for ($NEAR, Team tokens etc..)
            buyer_id, //the buyer and price are passed in incase something goes wrong and we need to refund the buyer
            sale,
            price,
            env::current_account_id(), //we are invoking this function on the current contract
            NO_DEPOSIT, //don't attach any deposit
            GAS_FOR_ROYALTIES, //GAS attached to the call to payout royalties
        ))
    }

    /*
        private method used to resolve the promise when calling nft_transfer_payout. This will take the payout object and 
        check to see if it's authentic and there's no problems. If everything is fine, it will pay the accounts. If there's a problem,
        it will refund the buyer for the price. 
    */
    #[private]
    pub fn resolve_purchase(
        &mut self,
        ft_token_id: AccountId,
        buyer_id: AccountId,
        sale: Sale,
        price: U128,
    ) -> U128 {
        // checking for payout information returned from the nft_transfer_payout method
        let payout_option = promise_result_as_success().and_then(|value| {
            //if we set the payout_option to None, that means something went wrong and we should refund the buyer
            near_sdk::serde_json::from_slice::<Payout>(&value)
                //converts the result to an optional value
                .ok()
                //returns None if the none. Otherwise executes the following logic
                .and_then(|payout_object| {
                    //we'll check if length of the payout object is > 10 or it's empty. In either case, we return None
                    if payout_object.payout.len() + sale.bids.len() > 10 || payout_object.payout.is_empty() {
                        env::log_str("Cannot have more than 10 royalties and sale.bids refunds");
                        None
                    
                    //if the payout object is the correct length, we move forward
                    } else {
                        //we'll keep track of how much the nft contract wants us to payout. Starting at the full price payed by the buyer
                        let mut remainder = price.0;
                        
                        //loop through the payout and subtract the values from the remainder. 
                        for &value in payout_object.payout.values() {
                            //checked sub checks for overflow or any errors and returns None if there are problems
                            remainder = remainder.checked_sub(value.0)?;
                        }
                        //Check to see if the NFT contract sent back a faulty payout that requires us to pay more or too little. 
                        //The remainder will be 0 if the payout summed to the total price. The remainder will be 1 if the royalties
                        //we something like 3333 + 3333 + 3333. 
                        if remainder == 0 || remainder == 1 {
                            //set the payout_option to be the payout because nothing went wrong
                            Some(payout_object.payout)
                        } else {
                            //if the remainder was anything but 1 or 0, we return None
                            None
                        }
                    }
                })
        });
        // if the payout option was some payout, we set this payout variable equal to that some payout
        let payout = if let Some(payout_option) = payout_option {
            payout_option
        //if the payout option was None, we refund the buyer for the price they payed and return
        } else {
            //If the ft token is NEAR, we simply refund the buyer for the price
            if ft_token_id == "near".parse().unwrap() {
                Promise::new(buyer_id).transfer(u128::from(price));
            }
            // This will be called from a ft_transfer_call so if we return the price, it will refund everything if the FT isn't NEAR
            return price;
        };
        // Going to payout everyone, first return all outstanding bids (accepted offer bid was already removed)
        self.refund_all_bids(&sale.bids);

        // NEAR payouts
        if ft_token_id == "near".parse().unwrap() {
            // Loop through and transfer all the users $NEAR
            for (receiver_id, amount) in payout {
                Promise::new(receiver_id).transfer(amount.0);
            }

            price
        } else {
            // FT payouts
            for (receiver_id, amount) in payout {
                ext_contract::ft_transfer(
                    receiver_id,
                    amount,
                    None,
                    ft_token_id.clone(),
                    1,
                    GAS_FOR_FT_TRANSFER,
                );
            }
            // keep all FTs (already transferred for payouts)
            U128(0)
        }
    }
}

/*
    This is the cross contract call that we call on our own contract. 
    private method used to resolve the promise when calling nft_transfer_payout. This will take the payout object and 
    check to see if it's authentic and there's no problems. If everything is fine, it will pay the accounts. If there's a problem,
    it will refund the buyer for the price. 
*/
#[ext_contract(ext_self)]
trait ExtSelf {
    fn resolve_purchase(
        &mut self,
        ft_token_id: AccountId,
        buyer_id: AccountId,
        sale: Sale,
        price: U128,
    ) -> Promise;
}
