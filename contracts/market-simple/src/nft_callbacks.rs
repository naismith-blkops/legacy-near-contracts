use crate::*;

/// approval callbacks from NFT Contracts

//struct for keeping track of the sale conditions for a Sale

#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct SaleArgs {
    // Map of FT to a price
    pub sale_conditions: SaleConditions,
    // Token type
    pub token_type: TokenType,
    // Don't serialize if it isn't an auction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_auction: Option<bool>,
}

/*
    trait that will be used as the callback from the NFT contract. When nft_approve is
    called, it will fire a cross contract call to this marketplace and this is the function
    that is invoked. 
*/
trait NonFungibleTokenApprovalsReceiver {
    fn nft_on_approve(
        &mut self,
        token_id: TokenId,
        owner_id: AccountId,
        approval_id: u64,
        msg: String,
    );
}

//implementation of the trait
#[near_bindgen]
impl NonFungibleTokenApprovalsReceiver for Contract {
    /// where we add the sale because we know nft owner can only call nft_approve
    fn nft_on_approve(
        &mut self,
        token_id: TokenId,
        owner_id: AccountId,
        approval_id: u64,
        msg: String,
    ) {
        // get the contract ID which is the predecessor
        let nft_contract_id = env::predecessor_account_id();
        //get the signer which is the person who initiated the transaction
        let signer_id = env::signer_account_id();

        //make sure that the signer isn't the predecessor. This is so that we're sure
        //this was called via a cross-contract call
        assert_ne!(
            nft_contract_id,
            signer_id,
            "nft_on_approve should only be called via cross-contract call"
        );
        //make sure the owner ID is the signer. 
        assert_eq!(
            owner_id,
            signer_id,
            "owner_id should be signer_id"
        );

        //we need to enforce that the user has enough storage for 1 EXTRA sale.  

        //get the storage for a sale. dot 0 converts from U128 to u128
        let storage_amount = self.storage_minimum_balance().0;
        //get the total storage paid by the owner
        let owner_paid_storage = self.storage_deposits.get(&signer_id).unwrap_or(0);
        //get the storage required which is simply the storage for the number of sales they have + 1 
        let signer_storage_required = (self.get_supply_by_owner_id(signer_id).0 + 1) as u128 * storage_amount;
        
        //make sure that the total paid is >= the required storage
        assert!(
            owner_paid_storage >= signer_storage_required,
            "Insufficient storage paid: {}, for {} sales at {} rate of per sale",
            owner_paid_storage, signer_storage_required / STORAGE_PER_SALE, STORAGE_PER_SALE
        );

        //if all these checks pass we can create the sale conditions object.
        let SaleArgs { sale_conditions, token_type, is_auction } =
            //the sale conditions come from the msg field. The market assumes that the user passed
            //in a proper msg. If they didn't, it panics. 
            near_sdk::serde_json::from_str(&msg).expect("Not valid SaleArgs");

        // Loop through all FTs passed into the sale conditions and make sure they're supported by the marketplace
        for (ft_token_id, _price) in sale_conditions.clone() {
            if !self.ft_token_ids.contains(&ft_token_id) {
                env::panic_str(
                    &format!("Token {} not supported by this market", ft_token_id),
                );
            }
        }

        // Default bids to an empty hash map
        let bids = HashMap::new();

        //create the unique sale ID which is the contract + DELIMITER + token ID
        let contract_and_token_id = format!("{}{}{}", nft_contract_id, DELIMETER, token_id);
                
        //insert the key value pair into the sales map. Key is the unique ID. value is the sale object
        self.sales.insert(
            &contract_and_token_id,
            &Sale {
                owner_id: owner_id.clone(),
                approval_id,
                nft_contract_id: nft_contract_id.clone(),
                token_id: token_id.clone(),
                sale_conditions,
                bids,
                created_at: U64(env::block_timestamp()/1000000),
                token_type: token_type.clone(),
                is_auction: is_auction.unwrap_or(false),
            },
        );

        //Extra functionality that populates collections necessary for the view calls 

        //get the sales by owner ID for the given owner. If there are none, we create a new empty set
        let mut by_owner_id = self.by_owner_id.get(&owner_id).unwrap_or_else(|| {
            UnorderedSet::new(
                StorageKey::ByOwnerIdInner {
                    //we get a new unique prefix for the collection by hashing the owner
                    account_id_hash: hash_account_id(&owner_id.to_string()),
                }
                .try_to_vec()
                .unwrap(),
            )
        });
        
        //insert the unique sale ID into the set
        by_owner_id.insert(&contract_and_token_id);
        //insert that set back into the collection for the owner
        self.by_owner_id.insert(&owner_id, &by_owner_id);

        //get the token IDs for the given nft contract ID. If there are none, we create a new empty set
        let mut by_nft_contract_id = self
            .by_nft_contract_id
            .get(&nft_contract_id)
            .unwrap_or_else(|| {
                UnorderedSet::new(
                    StorageKey::ByNFTContractIdInner {
                        //we get a new unique prefix for the collection by hashing the owner
                        account_id_hash: hash_account_id(&nft_contract_id.to_string()),
                    }
                    .try_to_vec()
                    .unwrap(),
                )
            });
        
        //insert the token ID into the set
        by_nft_contract_id.insert(&token_id);
        //insert the set back into the collection for the given nft contract ID
        self.by_nft_contract_id
            .insert(&nft_contract_id, &by_nft_contract_id);

        // If there is a token Type, ensure the token ID contains the token type and populate state for view calls
        if let Some(token_type) = token_type {
            assert!(token_id.contains(&token_type), "TokenType should be substr of TokenId");
            
            //get the token IDs for the given token type. If there are none, we create a new empty set
            let mut by_nft_token_type = self
                .by_nft_token_type
                .get(&token_type)
                .unwrap_or_else(|| {
                    UnorderedSet::new(
                        StorageKey::ByNFTTokenTypeInner {
                            //we get a new unique prefix for the collection by hashing the token type
                            token_type_hash: hash_account_id(&token_type),
                        }
                        .try_to_vec()
                        .unwrap(),
                    )
                });

            //insert the sales ID into the set
            by_nft_token_type.insert(&contract_and_token_id);
            //insert the set back into the collection for the given token type
            self.by_nft_token_type
                .insert(&token_type, &by_nft_token_type);
        }
    }
}
