# tensorust for DuckDB

Typed SQL tables over the [Tensorust Data API](https://shop.tensorust.com): what private credit lenders (BDCs) mark the same loan at, where they disagree, Q3 2026 markdowns as each 10-Q is filed, federal research awards that pay banned-list vendors, and the catalog of 145 entity-level datasets with their free samples.

```sql
INSTALL tensorust FROM community;
LOAD tensorust;

-- Loans two BDC managers mark 10+ points apart, widest gap first
SELECT company, gap_points, lowest_manager, lowest_mark, highest_manager, highest_mark
FROM tensorust_credit_disagreements();

-- Every fund's mark on one borrower's first-lien loan
FROM tensorust_lender_marks('Pluralsight');

-- Q3 2026 cuts of 5+ points, with holders that have not filed yet
SELECT company, fund, jun30_mark, sep30_mark, change_points, holders_not_filed_yet_above
FROM tensorust_q3_cuts();

-- Check your own book against the loans lenders disagree on
SELECT b.borrower, b.my_mark, d.lowest_mark, d.highest_mark, d.gap_points
FROM my_book b
JOIN tensorust_credit_disagreements() d ON lower(d.company) = lower(b.borrower);
```

## Functions

| Function | One row per | Free preview | With an enterprise key |
| --- | --- | --- | --- |
| `tensorust_datasets(search := '', category := '')` | dataset in the catalog | all datasets | same |
| `tensorust_sample(slug)` | row of the dataset's public sample | up to 25 rows | same |
| `tensorust_lender_marks(company)` | BDC fund holding the first-lien loan | up to 3 funds, marks only | every fund with par, prior-quarter mark, Sep 30 mark, filing link |
| `tensorust_credit_disagreements()` | loan marked 10+ points apart | top 10 | full list |
| `tensorust_q3_cuts()` | fund that cut a mark 5+ points in Q3 2026 | top 10 | every cut, with pending holders by name |
| `tensorust_exposure(institution)` | federal award with a banned-list vendor in its papers | 5 largest awards | every award with the evidence sentence and paper link |

Columns are typed: marks and dollars are `DOUBLE`, counts `BIGINT`, dates `DATE`. A figure the API does not publish stays `NULL`, never `0`. Every credit and exposure row carries an `access` column (`preview` or `full`).

`tensorust_sample` infers each column's type from the sample itself. Values with leading zeros (CIK, ZIP, FIPS) stay `VARCHAR`.

## API key

Previews need no key. For full rows, use an enterprise key from <https://shop.tensorust.com/offers/enterprise-api>:

```sql
SET tensorust_api_key = 'tdk_...';
```

or set the `TENSORUST_API_KEY` environment variable before starting DuckDB. The setting wins over the variable. The key is sent as an `Authorization: Bearer` header, never in a URL. `RESET tensorust_api_key` goes back to the preview.

Full datasets (not only the 25-row samples) are licensed yearly; see the `tiers` and `buy_url` columns of `tensorust_datasets()`.

## Data sources

Credit: SEC Business Development Company data sets (XBRL schedules of investments), first-lien term loans, plus each Q3 2026 10-Q as it is filed on EDGAR. Exposure: federal award records matched to 2025-26 papers (Europe PMC, OpenAlex) that name a vendor on the DoD 1260H, Section 889, FCC Covered List, UFLPA or BIOSECURE lists. Every row carries its `as_of` date.

## Building

```sh
git clone --recurse-submodules https://github.com/Mattbusel/duckdb-tensorust
cd duckdb-tensorust
make configure
make release
make test_release
```

Requires Rust, Python 3 and make. Tests call the live API without a key.

`TENSORUST_BASE_URL` points the extension at another deployment for testing.
