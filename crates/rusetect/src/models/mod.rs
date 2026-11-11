//! Tree model inference without native libraries.
pub mod lgbm;
pub mod xgb;

pub use lgbm::LiteBooster;
pub use xgb::XgbBooster;
