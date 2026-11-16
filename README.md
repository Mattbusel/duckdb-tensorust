# tensorust for DuckDB

A DuckDB extension that turns the [Tensorust Data API](https://shop.tensorust.com) into typed SQL tables: what private credit lenders (BDCs) mark the same loan at, where they disagree, Q3 2026 markdowns as each 10-Q is filed, federal research awards that pay banned-list vendors, and the catalog of 145 entity-level datasets with free samples. Every function answers a free preview without a key.

## Install

The extension is submitted to DuckDB community extensions ([duckdb/community-extensions#3638](https://github.com/duckdb/community-extensions/pull/3638)). Once it is listed:

```sql
INSTALL tensorust FROM community;
LOAD tensorust;
```

Until then, build it (needs Rust, Python 3 and make) and load the file into DuckDB v2.0.0 started with `duckdb -unsigned`:

```sh
git clone --recurse-submodules https://github.com/Mattbusel/duckdb-tensorust
cd duckdb-tensorust
make configure && make release
duckdb -unsigned -c "LOAD 'build/release/tensorust.duckdb_extension'; FROM tensorust_lender_marks('Pluralsight');"
```

## Example

```sql
-- Loans two BDC managers mark 10+ points apart, widest gap first
SELECT company, gap_points, lowest_manager, lowest_mark, highest_manager, highest_mark
FROM tensorust_credit_disagreements();

-- Every fund's mark on one borrower's first-lien loan
FROM tensorust_lender_marks('Pluralsight');
```

For full rows, `SET tensorust_api_key = 'tdk_...';` or set `TENSORUST_API_KEY` before starting DuckDB. Keys: https://shop.tensorust.com/offers/enterprise-api

## Docs and license

- Functions, columns and data sources: [docs/USAGE.md](https://github.com/Mattbusel/duckdb-tensorust/blob/main/docs/USAGE.md)
- MIT license: [LICENSE](https://github.com/Mattbusel/duckdb-tensorust/blob/main/LICENSE)
