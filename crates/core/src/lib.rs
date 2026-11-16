//! `tensorust`: the Tensorust Data API as typed DuckDB table functions.
//!
//! | Function | Rows |
//! |---|---|
//! | `tensorust_datasets(search := '', category := '')` | the 145-dataset catalog with prices and sample links |
//! | `tensorust_sample(slug)` | a dataset's free 25-row sample, typed |
//! | `tensorust_lender_marks(company)` | every BDC fund's mark on the company's first-lien loan |
//! | `tensorust_credit_disagreements()` | loans two or more BDC managers mark 10+ points apart |
//! | `tensorust_q3_cuts()` | Q3 2026 markdowns of 5+ points as each BDC files |
//! | `tensorust_exposure(institution)` | federal awards whose papers name a banned-list vendor |
//!
//! Without a key every function returns the free preview. With an enterprise
//! key (`SET tensorust_api_key = 'tdk_...'` or `TENSORUST_API_KEY`) the credit
//! and exposure functions return the full rows.

use duckdb_neo::{Result, extension::Extension};

pub mod api;
mod functions;
mod options;

/// The category every function of this extension is documented under in `duckdb_functions()`.
pub const CATEGORY: &str = "tensorust";

/// Register everything the extension provides.
pub fn register(extension: &Extension) -> Result<()> {
    options::register(extension)?;
    functions::register(extension)?;
    Ok(())
}
