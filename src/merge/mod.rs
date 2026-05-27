use std::collections::HashMap;

use sea_orm::{ConnectionTrait, DatabaseConnection};

use crate::db::helpers::portable_statement;
use crate::db::row_ext::QueryResultExt;
use crate::util::{now_unix, stable_item_id};

/// Union-Find data structure with path compression and union by rank.
pub struct UnionFind {
    parent: HashMap<String, String>,
    rank: HashMap<String, usize>,
}

impl UnionFind {
    pub fn new() -> Self {
        Self {
            parent: HashMap::new(),
            rank: HashMap::new(),
        }
    }

    pub fn make_set(&mut self, x: &str) {
        if !self.parent.contains_key(x) {
            self.parent.insert(x.to_string(), x.to_string());
            self.rank.insert(x.to_string(), 0);
        }
    }

    pub fn find(&mut self, x: &str) -> String {
        let parent = self.parent.get(x).cloned().unwrap_or_else(|| x.to_string());
        if parent == x {
            return x.to_string();
        }
        let root = self.find(&parent);
        self.parent.insert(x.to_string(), root.clone());
        root
    }

    pub fn union(&mut self, x: &str, y: &str) {
        let root_x = self.find(x);
        let root_y = self.find(y);
        if root_x == root_y {
            return;
        }
        let rank_x = *self.rank.get(&root_x).unwrap_or(&0);
        let rank_y = *self.rank.get(&root_y).unwrap_or(&0);
        if rank_x < rank_y {
            self.parent.insert(root_x, root_y);
        } else if rank_x > rank_y {
            self.parent.insert(root_y, root_x);
        } else {
            self.parent.insert(root_y, root_x.clone());
            self.rank.insert(root_x, rank_x + 1);
        }
    }

    pub fn groups(&mut self) -> HashMap<String, Vec<String>> {
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        let keys: Vec<String> = self.parent.keys().cloned().collect();
        for key in keys {
            let root = self.find(&key);
            groups.entry(root).or_default().push(key);
        }
        groups
    }
}

/// Find and merge duplicate movies across libraries using provider IDs.
pub async fn merge_multi_version(db: &DatabaseConnection) -> anyhow::Result<usize> {
    let backend = db.get_database_backend();

    // Query all movies with their TMDb provider IDs
    let rows = db
        .query_all(portable_statement(
            backend,
            r#"SELECT mi.id, mi.library_id, mi.size_bytes, pi.provider_item_id
               FROM media_items mi
               JOIN provider_ids pi ON pi.item_id = mi.id
               WHERE mi.item_type = 'Movie' AND mi.is_folder = 1
               AND pi.provider IN ('Tmdb', 'Imdb', 'Tvdb')
               ORDER BY pi.provider_item_id"#,
            vec![],
        ))
        .await?;

    if rows.is_empty() {
        return Ok(0);
    }

    // Group by provider_item_id
    let mut provider_groups: HashMap<String, Vec<(String, String, i64)>> = HashMap::new();
    for row in &rows {
        let id = row.get_str("id").unwrap_or_default();
        let library_id = row.get_str("library_id").unwrap_or_default();
        let size = row.get_i64("size_bytes").unwrap_or(0);
        let provider_id = row.get_str("provider_item_id").unwrap_or_default();
        provider_groups
            .entry(provider_id)
            .or_default()
            .push((id, library_id, size));
    }

    // Build union-find for items sharing the same provider ID within the same library
    let mut uf = UnionFind::new();
    for (_provider_id, items) in &provider_groups {
        if items.len() < 2 {
            continue;
        }
        for (id, _lib, _) in items {
            uf.make_set(id);
        }
        // Union all items in the group
        for i in 1..items.len() {
            uf.union(&items[0].0, &items[i].0);
        }
    }

    let groups = uf.groups();
    let merge_count = groups.values().filter(|g| g.len() > 1).count();

    if merge_count == 0 {
        return Ok(0);
    }

    let now = now_unix();
    for (_root, members) in &groups {
        if members.len() < 2 {
            continue;
        }

        // Pick representative (largest file)
        let representative = members
            .iter()
            .max_by_key(|id| {
                provider_groups
                    .values()
                    .flatten()
                    .find(|(iid, _, _)| iid == *id)
                    .map(|(_, _, s)| *s)
                    .unwrap_or(0)
            })
            .cloned()
            .unwrap_or_else(|| members[0].clone());

        for member in members {
            if *member == representative {
                continue;
            }

            // Get the provider ID for this member
            let provider_id = provider_groups
                .values()
                .flatten()
                .find(|(id, _, _)| id == member)
                .map(|(_, _, _)| {
                    provider_groups
                        .iter()
                        .find(|(_, items)| items.iter().any(|(id, _, _)| id == member))
                        .map(|(pid, _)| pid.clone())
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            let merge_id = stable_item_id(std::path::Path::new(&format!("merge:{representative}:{member}")));

            // Upsert merge group record
            db.execute(portable_statement(
                backend,
                "INSERT OR REPLACE INTO merge_groups (id, representative_id, member_id, provider, provider_item_id, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                vec![
                    merge_id.into(),
                    representative.clone().into(),
                    member.clone().into(),
                    "Tmdb".into(),
                    provider_id.into(),
                    now.into(),
                ],
            ))
            .await?;

            // Set parent_id of member to representative
            db.execute(portable_statement(
                backend,
                "UPDATE media_items SET parent_id = ? WHERE id = ? AND parent_id != ?",
                vec![
                    representative.clone().into(),
                    member.clone().into(),
                    representative.clone().into(),
                ],
            ))
            .await?;
        }
    }

    tracing::info!("merged {merge_count} multi-version groups");
    Ok(merge_count)
}
