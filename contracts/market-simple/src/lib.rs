use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::collections::{LookupMap, UnorderedMap, UnorderedSet};
use near_sdk::json_types::{U128, U64};
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{
    assert_one_yocto, env, ext_contract, near_bindgen, AccountId, Balance, Gas, PanicOnDefault,
    Promise, PromiseOrValue, CryptoHash, BorshStorageKey,
};
use std::collections::HashMap;

use crate::external::*;
use crate::internal::*;
use crate::sale::*;
use near_sdk::env::STORAGE_PRICE_PER_BYTE;

mod external;
mod ft_callbacks;
mod internal;
mod nft_callbacks;
mod sale;
mod sale_views;

//GAS constants to attach to calls
const GAS_FOR_FT_TRANSFER: Gas = Gas(5_000_000_000_000);
const GAS_FOR_ROYALTIES: Gas = Gas(115_000_000_000_000);
const GAS_FOR_NFT_TRANSFER: Gas = Gas(15_000_000_000_000);
const BID_HISTORY_LENGTH_DEFAULT: u8 = 1;

//constant used to attach 0 NEAR to a call
const NO_DEPOSIT: Balance = 0;
//the minimum storage to have a sale on the contract.
const STORAGE_PER_SALE: u128 = 1000 * STORAGE_PRICE_PER_BYTE;
//every sale will have a unique ID which is `CONTRACT + DELIMITER + TOKEN_ID`
static DELIMETER: &str = "||";

//Creating custom types to use within the contract. This makes things more readable. 
pub type SaleConditions = HashMap<FungibleTokenId, U128>;
pub type Bids = HashMap<FungibleTokenId, Vec<Bid>>;
pub type TokenId = String;
pub type TokenType = Option<String>;
pub type FungibleTokenId = AccountId;
pub type ContractAndTokenId = String;
//defines the payout type we'll be parsing from the NFT contract as a part of the royalty standard.
#[derive(Serialize, Deserialize)]
#[serde(crate = "near_sdk::serde")]
pub struct Payout {
    pub payout: HashMap<AccountId, U128>,
} 
#[derive(Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct StorageBalanceBounds {
    pub min: U128,
    pub max: Option<U128>,
}

#[near_bindgen]
#[derive(BorshDeserialize, BorshSerialize, PanicOnDefault)]
pub struct Contract {
    //keep track of the owner of the contract
    pub owner_id: AccountId,
    /*
        to keep track of the sales, we map the ContractAndTokenId to a Sale. 
        the ContractAndTokenId is the unique identifier for every sale. It is made
        up of the `contract ID + DELIMITER + token ID`
    */
    pub sales: UnorderedMap<ContractAndTokenId, Sale>,
    //keep track of all the Sale IDs for every account ID
    pub by_owner_id: LookupMap<AccountId, UnorderedSet<ContractAndTokenId>>,
    //keep track of all the token IDs for sale for a given contract
    pub by_nft_contract_id: LookupMap<AccountId, UnorderedSet<TokenId>>,
    //keep track of all the Sale IDs for a given token type
    pub by_nft_token_type: LookupMap<String, UnorderedSet<ContractAndTokenId>>,
    //keep track of all fungible token IDs that the marketplace holds
    pub ft_token_ids: UnorderedSet<FungibleTokenId>,
    //keep track of the storage that accounts have payed
    pub storage_deposits: LookupMap<AccountId, Balance>,
    //keep track of the length of the bid history
    pub bid_history_length: u8,
}

/// Helper structure to for keys of the persistent collections.
#[derive(BorshStorageKey, BorshSerialize)]
pub enum StorageKey {
    Sales,
    ByOwnerId,
    ByOwnerIdInner { account_id_hash: CryptoHash },
    ByNFTContractId,
    ByNFTContractIdInner { account_id_hash: CryptoHash },
    ByNFTTokenType,
    ByNFTTokenTypeInner { token_type_hash: CryptoHash },
    FTTokenIds,
    StorageDeposits,
}

#[near_bindgen]
impl Contract {
    /*
        initialization function (can only be called once).
        this initializes the contract with default data and the owner ID
        that's passed in in addition to optional 
    */
    #[init]
    pub fn new(owner_id: AccountId, ft_token_ids:Option<Vec<FungibleTokenId>>, bid_history_length:Option<u8>) -> Self {
        let mut this = Self {
            //set the owner_id field equal to the passed in owner_id. 
            owner_id: owner_id.into(),

            //Storage keys are simply the prefixes used for the collections. This helps avoid data collision
            sales: UnorderedMap::new(StorageKey::Sales),
            by_owner_id: LookupMap::new(StorageKey::ByOwnerId),
            by_nft_contract_id: LookupMap::new(StorageKey::ByNFTContractId),
            by_nft_token_type: LookupMap::new(StorageKey::ByNFTTokenType),
            ft_token_ids: UnorderedSet::new(StorageKey::FTTokenIds),
            storage_deposits: LookupMap::new(StorageKey::StorageDeposits),
            bid_history_length: bid_history_length.unwrap_or(BID_HISTORY_LENGTH_DEFAULT),
        };
        // support NEAR by default
        this.ft_token_ids.insert(&"near".parse().unwrap());
        
        // if ft token IDs were included, loop through and insert them into the map
        if let Some(ft_token_ids) = ft_token_ids {
            for ft_token_id in ft_token_ids {
                this.ft_token_ids.insert(&ft_token_id);
            }
        }

        //return the Contract object
        this
    }

    /// Owner only (add a set of fungible token IDs to be supported by the marketplace)
    pub fn add_ft_token_ids(&mut self, ft_token_ids: Vec<FungibleTokenId>) -> Vec<bool> {
        self.assert_owner();
        let mut added = vec![];
        for ft_token_id in ft_token_ids {
            added.push(self.ft_token_ids.insert(&ft_token_id));
        }
        added
    }

    /// Allows users to deposit storage. This is to cover the cost of storing sale objects on the contract
    /// Optional account ID is to users can pay for storage for other people.
    #[payable]
    pub fn storage_deposit(&mut self, account_id: Option<AccountId>) {
        //get the account ID to pay for storage for. 
        let storage_account_id = account_id.unwrap_or_else(env::predecessor_account_id);

        //get the deposit value which is how much the user wants to add to their storage
        let deposit = env::attached_deposit();

        //make sure the deposit is greater than or equal to the minimum storage for a sale
        assert!(
            deposit >= STORAGE_PER_SALE,
            "Requires minimum deposit of {}",
            STORAGE_PER_SALE
        );

        //get the balance of the account (if the account isn't in the map we default to a balance of 0)
        let mut balance: u128 = self.storage_deposits.get(&storage_account_id).unwrap_or(0);
        //add the deposit to their balance
        balance += deposit;
        //insert the balance back into the map for that account ID
        self.storage_deposits.insert(&storage_account_id, &balance);
    }

    //Allows users to withdraw any excess storage that they're not using. Say Bob pays 0.01N for 1 sale
    //Alice then buys Bob's token. This means bob has paid 0.01N for a sale that's no longer on the marketplace
    //Bob could then withdraw this 0.01N back into his account. 
    #[payable]
    pub fn storage_withdraw(&mut self) {
        //make sure the user attaches exactly 1 yoctoNEAR for security purposes.
        //this will redirect them to the NEAR wallet (or requires a full access key)
        assert_one_yocto();
        //the account to withdraw storage to is always the function caller
        let owner_id = env::predecessor_account_id();
        //get the amount that the user has by removing them from the map. If they're not in the map, default to 0
        let mut amount = self.storage_deposits.remove(&owner_id).unwrap_or(0);
        
        //how many sales is that user taking up currently. This returns a set
        let sales = self.by_owner_id.get(&owner_id);
        //get the length of that set. 
        let len = sales.map(|s| s.len()).unwrap_or_default();
        //how much NEAR is being used up for all the current sales on the account 
        let diff = u128::from(len) * STORAGE_PER_SALE;

        //the excess to withdraw is the total storage paid - storage being used up.
        amount -= diff;

        //if that excess to withdraw is > 0, we transfer the amount to the user.
        if amount > 0 {
            Promise::new(owner_id.clone()).transfer(amount);
        }
        //we need to add back the storage being used up into the map if it's greater than 0.
        //this is so that if the user had 500 sales on the market, we insert that value here so
        //if those sales get taken down, the user can then go and withdraw 500 sales worth of storage.
        if diff > 0 {
            self.storage_deposits.insert(&owner_id, &diff);
        }
    }

    /// views

    /// Return a list of supported fungible token IDs
    pub fn supported_ft_token_ids(&self) -> Vec<FungibleTokenId> {
        self.ft_token_ids.to_vec()
    }

    /// Returns the min and max amount of storage on the marketplace
    pub fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        StorageBalanceBounds {
            min: U128(STORAGE_PER_SALE),
            max: None,
        }
    }

    /// Return the minimum storage for 1 sale
    pub fn storage_minimum_balance(&self) -> U128 {
        U128(STORAGE_PER_SALE)
    }

    /// Return how much storage an account has paid for
    pub fn storage_balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.storage_deposits.get(&account_id).unwrap_or(0))
    }
}
