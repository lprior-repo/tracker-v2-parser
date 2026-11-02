use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, ensure};

type Partition<T> = Arc<BTreeMap<i64, (T, usize)>>;

#[derive(Clone, Debug)]
pub(super) struct Index<T> {
    partitions: [Partition<T>; 256],
    length: usize,
    bytes: usize,
}

impl<T> Default for Index<T> {
    fn default() -> Self {
        Self {
            partitions: std::array::from_fn(|_| Arc::new(BTreeMap::new())),
            length: 0,
            bytes: 0,
        }
    }
}

impl<T> Index<T> {
    fn partition(id: i64) -> usize {
        id.to_le_bytes().first().copied().map_or(0, usize::from)
    }

    pub const fn len(&self) -> usize {
        self.length
    }

    pub fn get(&self, id: i64) -> Option<&T> {
        self.partitions
            .get(Self::partition(id))?
            .get(&id)
            .map(|(value, _)| value)
    }

    pub fn contains_key(&self, id: i64) -> bool {
        self.get(id).is_some()
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.partitions
            .iter()
            .flat_map(|partition| partition.values().map(|(value, _)| value))
    }
}

impl<T: Clone> Index<T> {
    pub fn remove(&mut self, id: i64) -> Result<()> {
        let partition = self
            .partitions
            .get_mut(Self::partition(id))
            .context("coverage partition outside bound")?;
        if !partition.contains_key(&id) {
            return Ok(());
        }
        if let Some((_, bytes)) = Arc::make_mut(partition).remove(&id) {
            self.length = self
                .length
                .checked_sub(1)
                .context("coverage count underflow")?;
            self.bytes = self
                .bytes
                .checked_sub(bytes)
                .context("coverage byte underflow")?;
        }
        Ok(())
    }

    pub fn insert(&mut self, id: i64, value: T, bytes: usize) -> Result<()> {
        self.remove(id)?;
        let length = self
            .length
            .checked_add(1)
            .context("coverage count overflow")?;
        let total = self
            .bytes
            .checked_add(bytes)
            .context("coverage byte overflow")?;
        ensure!(
            length <= 250_000 && total <= 128 * 1024 * 1024,
            "compiled index exceeds page or byte budget"
        );
        let partition = self
            .partitions
            .get_mut(Self::partition(id))
            .context("coverage partition outside bound")?;
        Arc::make_mut(partition).insert(id, (value, bytes));
        self.length = length;
        self.bytes = total;
        Ok(())
    }
}
