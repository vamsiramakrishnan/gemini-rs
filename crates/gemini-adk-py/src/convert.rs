//! JSON values across the boundary: Python dicts, lists and scalars in,
//! the same out, with no string round trip.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use serde_json::Value;

/// A Python object as JSON. Anything `json.dumps` would refuse is a
/// `TypeError` naming the Python type.
pub fn to_json(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    pythonize::depythonize(obj).map_err(|e| {
        PyTypeError::new_err(format!(
            "not JSON-compatible ({}): {e}",
            obj.get_type()
                .name()
                .map(|n| n.to_string())
                .unwrap_or_default()
        ))
    })
}

/// A JSON value as a Python object.
pub fn to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    pythonize::pythonize(py, value).map_err(Into::into)
}

/// An optional Python object as an optional JSON value.
pub fn opt_to_json(obj: Option<&Bound<'_, PyAny>>) -> PyResult<Option<Value>> {
    obj.filter(|o| !o.is_none()).map(to_json).transpose()
}
