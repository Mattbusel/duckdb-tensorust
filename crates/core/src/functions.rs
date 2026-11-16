//! The six table functions. Each one fetches at bind time (so the column list
//! can depend on the data, as `tensorust_sample` needs), then `exec` emits the
//! fetched rows one chunk at a time.

use std::sync::atomic::{AtomicUsize, Ordering};

use duckdb_neo::{
    DuckDBType, Result,
    bind_arguments::BindArgument,
    connection::Context,
    data_chunk::DataChunkRef,
    error::Error,
    extension::Extension,
    signature::{FunctionDocumentationBuilder, FunctionParameter, SignatureBuilder},
    table_function::{
        BindFunctionHandle, ExecColumnInfo, InitColumnData, TableFunctionBuilder,
        TableFunctionCallbacks,
    },
    types::DateValue,
};

use crate::{
    CATEGORY,
    api::{self, Cell, Kind, Table},
    options,
};

/// Which API call a registered function makes.
#[derive(Clone, Copy)]
enum Source {
    Datasets,
    Sample,
    LenderMarks,
    Disagreements,
    Q3Cuts,
    Exposure,
}

struct ApiTable {
    name: &'static str,
    source: Source,
}

/// Read argument `index` as a string; SQL NULL and a missing argument read as `None`.
fn string_argument(arguments: &[BindArgument], index: usize) -> Result<Option<String>> {
    match arguments.get(index).and_then(|a| a.value.as_ref()) {
        Some(value) => value.get::<String>(),
        None => Ok(None),
    }
}

fn required(name: &str, what: &str, value: Option<String>) -> Result<String> {
    match value.map(|v| v.trim().to_string()) {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(Error::invalid_input(format!("{name}: pass {what}"))),
    }
}

fn logical(context: &Context, kind: Kind) -> Result<duckdb_neo::logical_type::LogicalType> {
    match kind {
        Kind::Varchar => String::logical_type(context),
        Kind::Double => f64::logical_type(context),
        Kind::Bigint => i64::logical_type(context),
        Kind::Boolean => bool::logical_type(context),
        Kind::Date => DateValue::logical_type(context),
    }
}

impl TableFunctionCallbacks for ApiTable {
    type BindData = Table;
    /// The next row to emit.
    type GlobalState = AtomicUsize;
    type LocalState = ();

    fn bind(
        &self,
        context: &Context,
        arguments: Vec<BindArgument>,
        bind_handle: BindFunctionHandle<'_>,
    ) -> Result<Self::BindData> {
        let key = options::api_key(context)?;
        let key = key.as_deref();
        let fetched = match self.source {
            Source::Datasets => api::datasets(
                &string_argument(&arguments, 0)?.unwrap_or_default(),
                &string_argument(&arguments, 1)?.unwrap_or_default(),
            ),
            Source::Sample => {
                let slug = required(self.name, "a dataset slug", string_argument(&arguments, 0)?)?;
                api::sample(&slug)
            }
            Source::LenderMarks => {
                let company = required(
                    self.name,
                    "a borrower name",
                    string_argument(&arguments, 0)?,
                )?;
                api::lender_marks(&company, key)
            }
            Source::Disagreements => api::credit_disagreements(key),
            Source::Q3Cuts => api::q3_cuts(key),
            Source::Exposure => {
                let name = required(
                    self.name,
                    "an institution name or UEI",
                    string_argument(&arguments, 0)?,
                )?;
                api::exposure(&name, key)
            }
        };
        let table = fetched.map_err(Error::api_error)?;
        for (name, kind) in &table.columns {
            bind_handle.add_result_column(name, &logical(context, *kind)?)?;
        }
        bind_handle.set_cardinality(table.rows.len(), true)?;
        Ok(table)
    }

    fn init_global_state(
        &self,
        _context: &Context,
        _bind_data: &Self::BindData,
        _column_data: InitColumnData<'_>,
    ) -> Result<Option<Self::GlobalState>> {
        Ok(Some(AtomicUsize::new(0)))
    }

    fn init_local_state(
        &self,
        _context: &Context,
        _bind_data: &Self::BindData,
        _global_state: Option<&Self::GlobalState>,
        _column_data: InitColumnData<'_>,
    ) -> Result<Option<Self::LocalState>> {
        Ok(None)
    }

    fn exec(
        &self,
        _context: &Context,
        table: &Self::BindData,
        _local_state: Option<&mut Self::LocalState>,
        global_state: Option<&Self::GlobalState>,
        mut output: DataChunkRef<'_>,
        _columns: ExecColumnInfo<'_>,
    ) -> Result<()> {
        let cursor =
            global_state.ok_or_else(|| Error::internal("tensorust: missing scan state"))?;
        let capacity = output.capacity()?;
        let start = cursor.fetch_add(capacity, Ordering::SeqCst);
        if start >= table.rows.len() {
            return Ok(());
        }
        let end = table.rows.len().min(start + capacity);
        let batch = &table.rows[start..end];

        for (position, (_, kind)) in table.columns.iter().enumerate() {
            macro_rules! fill {
                ($t:ty, $variant:ident, $v:ident => $conv:expr) => {{
                    let mut vector = output.get_vector_at_mut::<$t>(position)?;
                    vector.set_size(batch.len())?;
                    let mut writer = vector.writer()?;
                    for (row, cells) in batch.iter().enumerate() {
                        match &cells[position] {
                            Cell::$variant($v) => writer.set(row, $conv)?,
                            _ => writer.set_null(row)?,
                        }
                    }
                }};
            }
            match kind {
                Kind::Varchar => fill!(String, Str, v => v.as_str()),
                Kind::Double => fill!(f64, F64, v => v),
                Kind::Bigint => fill!(i64, I64, v => v),
                Kind::Boolean => fill!(bool, Bool, v => v),
                Kind::Date => {
                    let mut vector = output.get_vector_at_mut::<DateValue>(position)?;
                    vector.set_size(batch.len())?;
                    let mut writer = vector.writer()?;
                    for (row, cells) in batch.iter().enumerate() {
                        match &cells[position] {
                            Cell::Date(days) => writer.set(row, &DateValue(*days))?,
                            _ => writer.set_null(row)?,
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn register_one(
    extension: &Extension,
    name: &'static str,
    source: Source,
    signature: SignatureBuilder,
    description: &str,
    examples: &[(&str, &str)],
) -> Result<()> {
    let mut docs = FunctionDocumentationBuilder::new()
        .description(description)
        .add_category(CATEGORY);
    for (title, sql) in examples {
        docs = docs.add_example(*title, *sql);
    }
    TableFunctionBuilder::new(
        name,
        signature.documentation(docs),
        ApiTable { name, source },
    )
    .register(extension)
}

pub(crate) fn register(extension: &Extension) -> Result<()> {
    register_one(
        extension,
        "tensorust_datasets",
        Source::Datasets,
        SignatureBuilder::new()
            .param(FunctionParameter::with_default("search", String::new()))
            .param(FunctionParameter::with_default("category", String::new())),
        "The Tensorust catalog of entity-level datasets (money owed, at risk, mispriced or about to expire), one row per dataset with its price tiers and free sample link. Free.",
        &[
            (
                "Whole catalog",
                "SELECT slug, name, category FROM tensorust_datasets()",
            ),
            (
                "Search",
                "SELECT slug, name FROM tensorust_datasets(search := 'BDC')",
            ),
        ],
    )?;
    register_one(
        extension,
        "tensorust_sample",
        Source::Sample,
        SignatureBuilder::new().param(FunctionParameter::new::<String>("slug")),
        "A dataset's free 25-row sample with typed columns, so fields and joins can be checked before buying. Free.",
        &[(
            "BDC non-accruals",
            "FROM tensorust_sample('bdc-nonaccrual-watch')",
        )],
    )?;
    register_one(
        extension,
        "tensorust_lender_marks",
        Source::LenderMarks,
        SignatureBuilder::new().param(FunctionParameter::new::<String>("company")),
        "What each BDC fund marks a US company's first-lien loan at (share of par, 2026-06-30 10-Q, plus Sep 30 marks as filed), with the lowest and highest manager and the gap. Free: up to three funds. Enterprise key: every fund with par, prior mark, Q3 mark and filing link.",
        &[("One borrower", "FROM tensorust_lender_marks('Pluralsight')")],
    )?;
    register_one(
        extension,
        "tensorust_credit_disagreements",
        Source::Disagreements,
        SignatureBuilder::new(),
        "Loans two or more BDC managers mark 10+ points apart at 2026-06-30, widest gap first. Free: the top 10. Enterprise key: the full list.",
        &[(
            "Widest gaps",
            "SELECT company, gap_points, lowest_manager, highest_manager FROM tensorust_credit_disagreements()",
        )],
    )?;
    register_one(
        extension,
        "tensorust_q3_cuts",
        Source::Q3Cuts,
        SignatureBuilder::new(),
        "BDC funds that cut a first-lien loan's mark by 5+ points from Jun 30 to Sep 30 2026, pulled from each Q3 10-Q as it is filed, with holders that have not filed yet. Free: the top 10. Enterprise key: every cut and pending holder.",
        &[(
            "Biggest cuts",
            "SELECT company, fund, jun30_mark, sep30_mark, change_points FROM tensorust_q3_cuts()",
        )],
    )?;
    register_one(
        extension,
        "tensorust_exposure",
        Source::Exposure,
        SignatureBuilder::new().param(FunctionParameter::new::<String>("institution")),
        "Federal research awards of one recipient (name or UEI) whose 2025-26 papers name a vendor on a banned-vendor list (DoD 1260H, Section 889, FCC Covered List, UFLPA, BIOSECURE). Free: the 5 largest awards. Enterprise key: every award with its evidence sentence and paper link.",
        &[(
            "One university",
            "FROM tensorust_exposure('Stanford University')",
        )],
    )?;
    Ok(())
}
