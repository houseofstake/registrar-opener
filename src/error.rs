pub const ONLY_ADMIN: &str = "only the admin may call this";
pub const ONLY_OPERATOR: &str = "only the current operator may call this";
pub const ONLY_ADMIN_OR_OPERATOR: &str = "only the admin or the operator may call this";
pub const ONLY_SELF: &str = "only this account may call this";
pub const ONLY_PENDING_ADMIN: &str = "only the nominated admin may accept";
pub const NO_PENDING_ADMIN: &str = "no admin has been nominated";
pub const ADMIN_IS_OPERATOR: &str = "admin and operator must be different accounts";
pub const NOMINEE_IS_OPERATOR: &str = "the nominated admin is the operator, the roles would merge";
pub const ROLE_IS_SELF: &str = "neither role may be this account";
pub const ALREADY_INSTALLED: &str = "this account already runs the opener";
pub const NO_STATE: &str = "there is no state to migrate";
pub const ONE_YOCTO: &str = "exactly one yoctoNEAR must be attached";

pub const NO_BATCH: &str = "no batch with that id";
pub const BATCH_APPROVED: &str = "the batch is approved and can no longer be edited";
pub const BATCH_NOT_APPROVED: &str = "the batch has not been approved";
pub const BATCH_EMPTY: &str = "the batch holds no names";
pub const BATCH_LIVE: &str = "an approved batch still holding names cannot be discarded";
pub const BATCH_NOT_DISCARDED: &str = "discard the batch before forgetting what it held";
pub const TOO_MANY_BATCHES: &str = "discard a batch before drafting another";
pub const BATCH_IDS_EXHAUSTED: &str = "batch ids are exhausted, reusing one would alias its \
                                       stored names";
pub const DIGEST_MISMATCH: &str = "digest does not match the batch contents";
pub const NOT_IN_BATCH: &str = "name is not in this batch";

pub const EMPTY_NAMES: &str = "no names supplied";
pub const TOO_MANY_NAMES: &str = "more names than this call accepts";
pub const DUPLICATE_NAME: &str = "duplicate name in list";
pub const NOT_TOP_LEVEL: &str = "name is not a top level account";
pub const NAME_TOO_SHORT: &str = "top level name is short enough to be forgeable";
pub const NAME_TOO_LONG: &str = "name exceeds the account id limit";
pub const NAME_IS_IMPLICIT: &str = "an implicit account id is somebody's derived address";

pub const GAS_TOO_LOW: &str = "attach more gas or send fewer names";
pub const FUNDING_TOO_LOW: &str = "funding is below the account storage floor";
pub const DEPOSIT_MISMATCH: &str = "attached deposit must be the funding times the name count";

pub const OPEN_FAILED: &str = "the account was not created";
pub const UPGRADE_FAILED: &str = "the deploy did not land";
