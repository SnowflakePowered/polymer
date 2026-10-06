use std::{cmp::Ordering, sync::Arc};

use dashmap::DashMap;
use nucleo_matcher::{
    Config, Matcher, Utf32String,
    pattern::{CaseMatching, Normalization, Pattern},
};
use rayon::prelude::*;
use turso::Value;
use uuid::Uuid;

use super::{GameCatalog, GameCatalogError, GamePage, GameQuery, GameSort};

const RESOLVE_TITLES: &str = r#"
INSERT INTO game_titles (root_binary_id, platform, title, sort_title, added_key)
SELECT id, platform, title, lower(title), added_key FROM (
    SELECT id, platform, added_key,
        COALESCE(named, NULLIF(replace(path, rtrim(path, replace(path, '/', '')), ''), ''), lower(hex(id))) AS title
    FROM (
        SELECT root.id AS id, root.platform AS platform,
            COALESCE(
                (SELECT metadata.string_value FROM metadata
                 WHERE metadata.parent_binary_id = root.id AND metadata.key = 'meta.title' AND metadata.data_kind = 'string'
                 ORDER BY metadata.source, metadata.ordinal, metadata.id LIMIT 1),
                (SELECT metadata.string_value FROM binaries AS disc JOIN metadata ON metadata.parent_binary_id = disc.id
                 WHERE disc.parent_binary_id = root.id AND metadata.key = 'meta.title' AND metadata.data_kind = 'string'
                 ORDER BY disc.id, metadata.source, metadata.ordinal, metadata.id LIMIT 1),
                (SELECT metadata.string_value FROM metadata
                 WHERE metadata.parent_binary_id = root.id AND metadata.key > 'meta.title.' AND metadata.key < 'meta.title/'
                   AND metadata.data_kind = 'string'
                 ORDER BY metadata.key, metadata.source, metadata.ordinal, metadata.id LIMIT 1),
                (SELECT metadata.string_value FROM binaries AS disc JOIN metadata ON metadata.parent_binary_id = disc.id
                 WHERE disc.parent_binary_id = root.id AND metadata.key > 'meta.title.' AND metadata.key < 'meta.title/'
                   AND metadata.data_kind = 'string'
                 ORDER BY disc.id, metadata.key, metadata.source, metadata.ordinal, metadata.id LIMIT 1),
                (SELECT metadata.string_value FROM metadata
                 WHERE metadata.parent_binary_id = root.id AND metadata.key = 'package.internalname' AND metadata.data_kind = 'string'
                 ORDER BY metadata.source, metadata.ordinal, metadata.id LIMIT 1),
                (SELECT metadata.string_value FROM binaries AS disc JOIN metadata ON metadata.parent_binary_id = disc.id
                 WHERE disc.parent_binary_id = root.id AND metadata.key = 'package.internalname' AND metadata.data_kind = 'string'
                 ORDER BY disc.id, metadata.source, metadata.ordinal, metadata.id LIMIT 1)
            ) AS named,
            COALESCE(
                (SELECT launchable.path FROM launchables AS launchable
                 WHERE launchable.binary_id = root.id ORDER BY launchable.path LIMIT 1),
                (SELECT launchable.path FROM binaries AS disc JOIN launchables AS launchable ON launchable.binary_id = disc.id
                 WHERE disc.parent_binary_id = root.id ORDER BY disc.id, launchable.path LIMIT 1)
            ) AS path,
            COALESCE(
                (SELECT launchable.id FROM launchables AS launchable WHERE launchable.binary_id = root.id
                 ORDER BY launchable.id LIMIT 1),
                (SELECT launchable.id FROM binaries AS disc JOIN launchables AS launchable ON launchable.binary_id = disc.id
                 WHERE disc.parent_binary_id = root.id ORDER BY launchable.id LIMIT 1)
            ) AS added_key
        FROM {roots}
        WHERE root.parent_binary_id IS NULL
    )
) WHERE added_key IS NOT NULL;
"#;

struct TitleEntry {
    platform: Arc<str>,
    sort_title: Arc<str>,
    haystack: Utf32String,
}

type TitleMatch = (u32, Arc<str>, Uuid);

#[derive(Default)]
pub(super) struct TitleIndex {
    entries: DashMap<Uuid, TitleEntry>,
    sync: tokio::sync::Mutex<()>,
}

impl TitleIndex {
    fn apply(&self, changes: Vec<(Uuid, Option<(String, String)>)>) {
        for (root, entry) in changes {
            match entry {
                Some((platform, title)) => self.entries.insert(
                    root,
                    TitleEntry {
                        platform: platform.into(),
                        sort_title: title.to_lowercase().into(),
                        haystack: title.into(),
                    },
                ),
                None => self.entries.remove(&root).map(|(_, entry)| entry),
            };
        }
    }

    fn search(&self, text: &str, platforms: &[String], end: usize) -> (Vec<Uuid>, usize) {
        let pattern = Pattern::parse(text, CaseMatching::Ignore, Normalization::Smart);
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        let mut matches = self
            .entries
            .par_iter()
            .filter(|entry| {
                platforms.is_empty()
                    || platforms
                        .iter()
                        .any(|platform| **platform == *entry.platform)
            })
            .map_init(
                || Matcher::new(config.clone()),
                |matcher, entry| {
                    pattern
                        .score(entry.haystack.slice(..), matcher)
                        .map(|score| (score, entry.sort_title.clone(), *entry.key()))
                },
            )
            .flatten()
            .collect::<Vec<TitleMatch>>();
        let order = |a: &TitleMatch, b: &TitleMatch| -> Ordering {
            b.0.cmp(&a.0)
                .then_with(|| a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
        };
        let total = matches.len();
        if end < total {
            matches.select_nth_unstable_by(end, order);
            matches.truncate(end);
        }
        matches.sort_unstable_by(order);
        (
            matches.into_iter().map(|(_, _, root)| root).collect(),
            total,
        )
    }
}

impl GameCatalog {
    pub async fn query_games(&self, query: GameQuery) -> Result<GamePage, GameCatalogError> {
        self.sync_titles(false).await?;
        let text = query
            .text
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_owned();
        let (roots, total) = if text.is_empty() {
            self.sorted_roots(&query).await?
        } else {
            let titles = self.titles.clone();
            let platforms = query.platforms.clone();
            let end = query.offset.saturating_add(query.limit);
            let (matches, total) =
                tokio::task::spawn_blocking(move || titles.search(&text, &platforms, end))
                    .await
                    .map_err(|error| {
                        GameCatalogError::InvalidData(format!("title search failed: {error}"))
                    })?;
            (matches.into_iter().skip(query.offset).collect(), total)
        };
        Ok(GamePage {
            games: self
                .games_by_ids(&roots, query.metadata_keys.as_deref())
                .await?,
            total,
        })
    }

    pub async fn game_platforms(&self) -> Result<Vec<String>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT DISTINCT platform FROM game_titles ORDER BY platform",
                (),
            )
            .await?;
        let mut platforms = Vec::new();
        while let Some(row) = rows.next().await? {
            platforms.push(row.get(0)?);
        }
        Ok(platforms)
    }

    async fn sorted_roots(
        &self,
        query: &GameQuery,
    ) -> Result<(Vec<Uuid>, usize), GameCatalogError> {
        let direction = if query.descending { "DESC" } else { "ASC" };
        let columns: &[&str] = match query.sort {
            GameSort::Title => &["sort_title", "root_binary_id"],
            GameSort::Platform => &["platform", "sort_title", "root_binary_id"],
            GameSort::Added => &["added_key", "root_binary_id"],
        };
        let order = columns
            .iter()
            .map(|column| format!("{column} {direction}"))
            .collect::<Vec<_>>()
            .join(", ");
        let filter = match query.platforms.len() {
            0 => String::new(),
            count => format!(
                "WHERE platform IN ({})",
                (1..=count)
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let mut parameters = query
            .platforms
            .iter()
            .cloned()
            .map(Value::from)
            .collect::<Vec<_>>();
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                &format!("SELECT COUNT(*) FROM game_titles {filter}"),
                parameters.clone(),
            )
            .await?;
        let total = rows
            .next()
            .await?
            .map(|row| row.get::<i64>(0))
            .transpose()?
            .unwrap_or_default();
        drop(rows);
        let count = parameters.len();
        parameters.push(Value::from(i64::try_from(query.limit).unwrap_or(i64::MAX)));
        parameters.push(Value::from(i64::try_from(query.offset).unwrap_or(i64::MAX)));
        let mut rows = connection
            .query(
                &format!(
                    "SELECT root_binary_id FROM game_titles {filter} ORDER BY {order} LIMIT ?{} OFFSET ?{}",
                    count + 1,
                    count + 2
                ),
                parameters,
            )
            .await?;
        let mut roots = Vec::new();
        while let Some(row) = rows.next().await? {
            roots.push(Self::uuid(row.get_value(0)?)?);
        }
        Ok((roots, usize::try_from(total).unwrap_or_default()))
    }

    pub(super) async fn sync_titles(&self, full: bool) -> Result<(), GameCatalogError> {
        let _sync = self.titles.sync.lock().await;
        let mut connection = self.connection().await?;
        let mut pending = connection
            .query("SELECT 1 FROM game_title_changes LIMIT 1", ())
            .await?;
        if !full && pending.next().await?.is_none() {
            return Ok(());
        }
        drop(pending);
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        transaction
            .execute(
                "DELETE FROM game_titles WHERE root_binary_id IN (SELECT root_binary_id FROM game_title_changes)",
                (),
            )
            .await?;
        transaction
            .execute(
                &RESOLVE_TITLES.replace(
                    "{roots}",
                    "game_title_changes AS changes CROSS JOIN binaries AS root ON root.id = changes.root_binary_id",
                ),
                (),
            )
            .await?;
        let mut rows = transaction
            .query(
                if full {
                    "SELECT root_binary_id, platform, title FROM game_titles"
                } else {
                    r#"SELECT changes.root_binary_id, titles.platform, titles.title
                       FROM game_title_changes AS changes
                       LEFT JOIN game_titles AS titles ON titles.root_binary_id = changes.root_binary_id"#
                },
                (),
            )
            .await?;
        let mut changes = Vec::new();
        while let Some(row) = rows.next().await? {
            let entry = match Self::optional_text(row.get_value(2)?)? {
                Some(title) => Some((row.get(1)?, title)),
                None => None,
            };
            changes.push((Self::uuid(row.get_value(0)?)?, entry));
        }
        drop(rows);
        transaction
            .execute("DELETE FROM game_title_changes", ())
            .await?;
        transaction.commit().await?;
        self.titles.apply(changes);
        Ok(())
    }
}
