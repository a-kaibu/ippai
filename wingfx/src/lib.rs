//! wingfx: conformance tests for the Windows graphics APIs (README.md, SPEC.md).
//!
//! A case file (cases/<suite>.txt) is a list of cases, each a short program of API calls in a
//! line language. The executor (runner/wingfx.c) runs it on a real system and prints what the
//! calls did; Windows' output is the reference (golden). The models here are executable
//! specifications of the same calls, written from Microsoft's documentation and corrected only
//! by Windows' recorded answers: they print what Windows should print. So
//!   * the model is checked against Windows (every recorded case),
//!   * any implementation (sake, Wine, ...) is checked against Windows' recorded answers, and
//!     against the model for as many fresh generated cases as wanted, with no Windows at hand.

pub mod case;
pub mod compare;
pub mod gen;
pub mod model;
pub mod rng;
pub mod spec;
