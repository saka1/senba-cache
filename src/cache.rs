pub trait Cache<K, V> {
    fn new(capacity: usize) -> Self
    where
        Self: Sized;
    fn capacity(&self) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// hit 時に visited bit を立てる必要があるので &mut self
    fn get(&mut self, key: &K) -> Option<&V>;

    /// 容量超過時に追い出された (K,V) を返す。oracle 比較の主データ。
    fn insert(&mut self, key: K, value: V) -> Option<(K, V)>;

    fn contains_key(&self, key: &K) -> bool;
}
