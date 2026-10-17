//! A small type map of shared host services, so op handlers in any crate can reach the database,
//! the platform and so on without the IPC crate knowing their types.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default)]
pub struct Services {
    map: HashMap<TypeId, Arc<dyn Any + Send + Sync>>,
}

impl Services {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a service. Registering the same type twice replaces it.
    pub fn insert<T: Any + Send + Sync>(&mut self, value: Arc<T>) {
        self.map.insert(TypeId::of::<T>(), value);
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.map
            .get(&TypeId::of::<T>())
            .cloned()
            .and_then(|a| a.downcast::<T>().ok())
    }

    /// Like [`get`](Self::get), as an op error when missing (a wiring bug, never user error).
    pub fn need<T: Any + Send + Sync>(&self) -> Result<Arc<T>, crate::ApiError> {
        self.get::<T>().ok_or_else(|| {
            crate::ApiError::internal(format!("service missing: {}", std::any::type_name::<T>()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_what_was_inserted() {
        let mut s = Services::new();
        s.insert(Arc::new(5u32));
        assert_eq!(*s.get::<u32>().unwrap(), 5);
        assert!(s.get::<u64>().is_none());
        assert!(s.need::<u64>().is_err());
    }
}
