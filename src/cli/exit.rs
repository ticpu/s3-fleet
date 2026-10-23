//! Exit codes, the part of the CLI contract scripts branch on.
//!
//! | code | meaning                                                         |
//! |------|-----------------------------------------------------------------|
//! | 0    | success                                                         |
//! | 1    | any other error (I/O, HTTP status, parse, discovery)            |
//! | 2    | usage error, or `setup` without a TTY                           |
//! | 3    | partial: `--keep-going` committed some hosts, others failed     |
//! | 4    | conflict: someone else wrote first, nothing was written; rerun  |
//! | 5    | the fleet does not exist, or is not public and we are anonymous |
//! | 6    | credentials missing or refused                                  |
//! | 7    | a host's key differs from the pinned one; `--rekey` to accept   |
//! | 8    | S3 unreachable, and no usable cache (or `--allow-stale` absent) |
//!
//! Codes are stable once released. Every error maps to exactly one code
//! through one function, and a test walks every variant.
