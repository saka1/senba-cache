use crate::error::Result;

pub trait Cache<K, V> {
    fn get(&self, key: &K) -> Result<&V>;
    fn set(&mut self, key: K, value: V) -> Result<()>;
    fn delete(&mut self, key: &K) -> Result<()>;
    fn clear(&mut self);
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
