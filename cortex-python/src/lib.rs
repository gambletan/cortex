use cortex_core::Cortex;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

/// Convert CortexError to PyErr.
fn to_py_err(e: cortex_core::CortexError) -> PyErr {
    PyRuntimeError::new_err(e.to_string())
}

/// Main Cortex Python binding.
#[pyclass]
struct PyCortex {
    inner: Cortex,
}

#[pymethods]
impl PyCortex {
    /// Open or create a Cortex database.
    #[new]
    fn new(db_path: &str) -> PyResult<Self> {
        let inner = Cortex::open(db_path).map_err(to_py_err)?;
        Ok(Self { inner })
    }

    /// Ingest a new memory from any channel.
    #[pyo3(signature = (text, channel, user_id=None, salience=None))]
    fn ingest(
        &self,
        text: &str,
        channel: &str,
        user_id: Option<&str>,
        salience: Option<f32>,
    ) -> PyResult<String> {
        let mem = self
            .inner
            .ingest(text, channel, user_id, salience, None)
            .map_err(to_py_err)?;
        Ok(mem.id.to_string())
    }

    /// Multi-signal retrieval. Returns list of (memory_id, score, content_summary).
    #[pyo3(signature = (query, limit, channel=None, person_id=None))]
    fn retrieve(
        &self,
        query: &str,
        limit: usize,
        channel: Option<&str>,
        person_id: Option<&str>,
    ) -> PyResult<Vec<(String, f32, String)>> {
        let pid = person_id
            .map(|s| uuid::Uuid::parse_str(s))
            .transpose()
            .map_err(|e| PyRuntimeError::new_err(format!("Invalid UUID: {}", e)))?;

        let results = self
            .inner
            .retrieve(query, limit, channel, pid, None)
            .map_err(to_py_err)?;

        Ok(results
            .into_iter()
            .map(|r| {
                let summary = match &r.memory.content {
                    cortex_core::types::MemContent::Text(t) => t.clone(),
                    cortex_core::types::MemContent::Fact { subject, predicate, object } => {
                        format!("{} {} {}", subject, predicate, object)
                    }
                    cortex_core::types::MemContent::Preference { key, value, .. } => {
                        format!("{} = {}", key, value)
                    }
                    other => format!("{:?}", other),
                };
                (r.memory.id.to_string(), r.score, summary)
            })
            .collect())
    }

    /// Generate LLM-ready context from memory state.
    #[pyo3(signature = (max_tokens, channel=None, person_id=None))]
    fn get_context(
        &self,
        max_tokens: usize,
        channel: Option<&str>,
        person_id: Option<&str>,
    ) -> PyResult<String> {
        let pid = person_id
            .map(|s| uuid::Uuid::parse_str(s))
            .transpose()
            .map_err(|e| PyRuntimeError::new_err(format!("Invalid UUID: {}", e)))?;

        self.inner
            .get_context(max_tokens, channel, pid)
            .map_err(to_py_err)
    }

    /// Add or resolve a person by channel identity.
    fn add_person(
        &self,
        name: &str,
        channel: &str,
        channel_user_id: &str,
    ) -> PyResult<String> {
        let person = self
            .inner
            .add_person(name, channel, channel_user_id)
            .map_err(to_py_err)?;
        Ok(person.id.to_string())
    }

    /// Run a consolidation cycle. Returns (episodes_scanned, promoted, swept, patterns).
    fn run_consolidation(&self) -> PyResult<(usize, usize, usize, usize)> {
        let report = self.inner.run_consolidation().map_err(to_py_err)?;
        Ok((
            report.episodes_scanned,
            report.promoted_to_semantic,
            report.decayed_swept,
            report.patterns_detected,
        ))
    }

    /// Get beliefs above a confidence threshold.
    fn get_beliefs(&self, threshold: f32) -> PyResult<Vec<(String, f32)>> {
        let beliefs = self.inner.get_beliefs(threshold).map_err(to_py_err)?;
        Ok(beliefs.into_iter().map(|b| (b.key, b.probability)).collect())
    }

    /// Observe evidence for a belief key.
    fn observe_belief(
        &self,
        key: &str,
        supports: bool,
        strength: f32,
    ) -> PyResult<f32> {
        let belief = self
            .inner
            .observe_belief(key, supports, strength)
            .map_err(to_py_err)?;
        Ok(belief.probability)
    }

    /// Add a semantic fact.
    fn add_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        confidence: f32,
        channel: &str,
    ) -> PyResult<String> {
        let mem = self
            .inner
            .add_fact(subject, predicate, object, confidence, channel, None)
            .map_err(to_py_err)?;
        Ok(mem.id.to_string())
    }

    /// Add a user preference.
    fn add_preference(
        &self,
        key: &str,
        value: &str,
        confidence: f32,
    ) -> PyResult<String> {
        let mem = self
            .inner
            .add_preference(key, value, confidence)
            .map_err(to_py_err)?;
        Ok(mem.id.to_string())
    }
}

#[pymodule]
fn cortex_python(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCortex>()?;
    Ok(())
}
