//! Disjoint set over file indices.

pub struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    pub fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }

    pub fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    pub fn union(&mut self, a: usize, b: usize) -> bool {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return false;
        }
        let (hi, lo) = if self.rank[ra] < self.rank[rb] {
            (rb, ra)
        } else {
            (ra, rb)
        };
        self.parent[lo] = hi;
        if self.rank[hi] == self.rank[lo] {
            self.rank[hi] += 1;
        }
        true
    }

    /// Members of each set that has more than one element, plus singletons,
    /// keyed by representative.
    pub fn groups(&mut self) -> std::collections::BTreeMap<usize, Vec<usize>> {
        let mut out: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for i in 0..self.parent.len() {
            let r = self.find(i);
            out.entry(r).or_default().push(i);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_transitively() {
        let mut u = UnionFind::new(5);
        assert!(u.union(0, 1));
        assert!(u.union(1, 2));
        assert!(!u.union(0, 2), "уже в одном множестве");
        assert_eq!(u.find(0), u.find(2));
        assert_ne!(u.find(0), u.find(3));

        let g = u.groups();
        let sizes: Vec<usize> = g.values().map(|v| v.len()).collect();
        assert_eq!(sizes.iter().sum::<usize>(), 5);
        assert!(sizes.contains(&3));
    }
}
