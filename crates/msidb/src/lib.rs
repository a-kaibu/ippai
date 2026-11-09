//! Windows Installer databases, written from the file format and Windows' observable
//! behaviour (Wine's msi conformance tests, and checks against real Windows in tests/winref):
//!
//! - `db`: the compound file (cfb) holding the string pool, the tables (column-major rows of
//!   raw values: string ids, integers offset by 0x8000 / 0x80000000), streams, storages and the
//!   summary information; rows kept sorted by their raw key values as Windows does.
//! - `sql`: MSI SQL (SELECT/INSERT/UPDATE/DELETE/CREATE/ALTER/DROP) parsed and run as views.
//! - `record`, `suminfo`, `format` (MsiFormatRecord), `cond` (MsiEvaluateCondition).
//!
//! Errors are Win32 error codes (what the msi.dll functions return).

pub mod cond;
pub mod db;
pub mod format;
pub mod idt;
pub mod names;
pub mod pool;
pub mod record;
pub mod sql;
pub mod suminfo;
pub mod transform;
pub mod view;

pub use db::{Col, Database, Table};
pub use record::{Field, Record};
pub use view::View;

pub(crate) use std::collections::HashMap;

pub type R<T> = Result<T, u32>;

pub const ERROR_SUCCESS: u32 = 0;
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
pub const ERROR_OPEN_FAILED: u32 = 110;
pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_INVALID_HANDLE: u32 = 6;
pub const ERROR_INVALID_DATA: u32 = 13;
pub const ERROR_OUTOFMEMORY: u32 = 14;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
pub const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
pub const ERROR_BAD_PATHNAME: u32 = 161;
pub const ERROR_ALREADY_EXISTS: u32 = 183;
pub const ERROR_MORE_DATA: u32 = 234;
pub const ERROR_NO_MORE_ITEMS: u32 = 259;
pub const ERROR_DATATYPE_MISMATCH: u32 = 1629;
pub const ERROR_FUNCTION_FAILED: u32 = 1627;
pub const ERROR_INVALID_FIELD: u32 = 1616;
pub const ERROR_INVALID_HANDLE_STATE: u32 = 1609;
pub const ERROR_BAD_QUERY_SYNTAX: u32 = 1615;
pub const ERROR_INVALID_TABLE: u32 = 1628;
pub const ERROR_CREATE_FAILED: u32 = 1631;
pub const ERROR_INSTALL_PACKAGE_INVALID: u32 = 1620;
pub const ERROR_INSTALL_PACKAGE_OPEN_FAILED: u32 = 1619;
pub const ERROR_INVALID_DATATYPE: u32 = 1804;
pub const ERROR_UNKNOWN_PROPERTY: u32 = 1608;
pub const ERROR_DIRECTORY: u32 = 267;
pub const ERROR_NOT_FOUND: u32 = 1168;
pub const ERROR_UNKNOWN_PRODUCT: u32 = 1605;
pub const ERROR_UNKNOWN_FEATURE: u32 = 1606;
pub const ERROR_UNKNOWN_COMPONENT: u32 = 1607;
pub const ERROR_INSTALL_FAILURE: u32 = 1603;
pub const ERROR_INSTALL_USEREXIT: u32 = 1602;
pub const ERROR_FUNCTION_NOT_CALLED: u32 = 1626;
pub const ERROR_INVALID_TRANSFORM: u32 = 1621;
pub const ERROR_OPEN_PACKAGE: u32 = ERROR_INSTALL_PACKAGE_OPEN_FAILED;

/// Integer NULL in records and conditions (MSI_NULL_INTEGER).
pub const NULL_INT: i32 = i32::MIN;
