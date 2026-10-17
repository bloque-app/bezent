// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The reserve book (`T-004-01`; `REQ-BK-7`, `INV-17`, `REQ-AS-9`, `REQ-BK-4`, `REQ-BE-6`).
//!
//! # Movements and their postings
//!
//! Debits raise reserve and value in transit; credits raise supply, owed mints, owed payouts and
//! surplus. Every entry balances, so the book keeps the reserve equation by construction, and the
//! check (`REQ-BK-8`) compares each of its terms with the networks.
//!
//! | Movement | Posted from | Debit | Credit |
//! |---|---|---|---|
//! | `arrival`, `conversion` | a final arrival of an admitted origin token at a reserve address (a conversion's output is one, `REQ-CV-2`) | reserve | owed mint (the arrival) |
//! | `mint` | the final `Minted` event of a deposit | owed mint (the arrival, in full) | supply (what was minted); surplus (the remainder below one minor unit of the asset, `REQ-AS-9`) |
//! | `redemption` | the final `Redeemed` event | supply | owed payout (the withdrawal) |
//! | `payout` | the final payout leg of a withdrawal, from the reserve | owed payout | reserve |
//! | `refund-mint` | the final `RefundMinted` event | owed payout | supply |
//! | `unassociated-refund` | the final payout leg refunding an unassociated arrival | owed mint (the arrival) | reserve |
//! | `surplus-contribution` | a final arrival at a reserve address from an address the operator declared it funds the reserve from (`surplus_funding_addresses`) | reserve | surplus (the arrival) |
//! | `surplus-withdrawal` | a final outflow from a reserve address the operator records | surplus | reserve |
//! | `network-cost`, `venue-fee` | a cost paid from a reserve position (`REQ-BK-4`) | surplus | reserve |
//! | `reversal` | an attesting party's report that a payment it reported final was reversed (`REQ-BE-6`) | surplus | reserve |
//! | `restoration` | the attesting party's report that it restored that reserve | reserve | surplus |
//! | `correction` | a new entry naming the one it corrects (`INV-17`) | as stated | as stated |
//!
//! A movement observed on chain is posted once, from its final observation, resting on the
//! observation's own block, never from a transfer's state: an unassociated arrival is owed mint
//! too (§5.3), and the check can then read the book as of the same final point it reads the network
//! at. Nothing is posted from an observation that is not final (`INV-16`), and no fee comes out of
//! a delivery (`REQ-TR-5`): a cost is a posting against surplus, never a smaller payout.
//!
//! An entry is never built with fewer than two non-zero postings: a movement of zero (anyone can
//! send zero of a token to a reserve address) is kept as the observation it is and posts nothing.
//! A final observation the store refuses to book is quarantined, once: it pages a human and pauses
//! its asset's routes (`REQ-IC-2`), and the rest of the book is posted as usual, so one bad
//! observation never stops the core or the reserve check (`INV-1`; review M1).

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};

use super::check::{LogPager, Page, Pager};
use super::{Asset, FinalPoint, Terms, coordinate_key};
use crate::refs::{self, Coordinate};
use crate::store::{self, Observation};

/// The item a posting to an itemised term carries when none was recorded: it never matches a
/// re-derived item, so an unitemised balance is a divergence, never a residual (`REQ-BK-2`).
pub const UNITEMISED: &str = "(unitemised)";

/// A side of a posting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Raises reserve or value in transit; lowers the others.
    Debit,
    /// Raises supply, owed mints, owed payouts or surplus; lowers the others.
    Credit,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Self::Debit => "debit",
            Self::Credit => "credit",
        }
    }
}

/// One posting of an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Posting {
    /// `reserve`, `in-transit`, `owed-mint`, `owed-payout`, `surplus` or `supply`.
    pub account: &'static str,
    /// The network, for a reserve posting.
    pub network: Option<String>,
    /// The side.
    pub side: Side,
    /// The amount, in book units.
    pub amount: u128,
    /// The item it belongs to (`REQ-BK-2`).
    pub item: Option<String>,
}

impl Posting {
    fn new(account: &'static str, side: Side, amount: u128) -> Self {
        Self {
            account,
            network: None,
            side,
            amount,
            item: None,
        }
    }
    fn on(mut self, network: &str) -> Self {
        self.network = Some(network.to_owned());
        self
    }
    fn item(mut self, item: &str) -> Self {
        self.item = Some(item.to_owned());
        self
    }
}

/// An entry to record: one movement and its postings, all of one asset.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The movement (`book_entries.kind`).
    pub kind: &'static str,
    /// Its reference: the coordinate of the movement, or the reference it was recorded with.
    pub reference: String,
    /// A note for people.
    pub memo: String,
    /// The final point it rests on, when it was observed on chain.
    pub point: Option<FinalPoint>,
    /// The observation it posts.
    pub observation: Option<i64>,
    /// The transfer it belongs to.
    pub transfer: Option<i64>,
    /// The entry it corrects (`INV-17`).
    pub corrects: Option<i64>,
    /// The asset.
    pub asset: String,
    /// Its postings; zero amounts are left out.
    pub postings: Vec<Posting>,
}

/// Why a recorded movement was not booked.
#[derive(Debug)]
pub enum BookError {
    /// The movement does not fit what the book holds; the text says why, with its spec id.
    Refused(String),
    /// The store failed.
    Store(sqlx::Error),
}

impl fmt::Display for BookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "refused: {why}"),
            Self::Store(e) => write!(f, "store: {e}"),
        }
    }
}

impl std::error::Error for BookError {}

impl From<sqlx::Error> for BookError {
    fn from(e: sqlx::Error) -> Self {
        Self::Store(e)
    }
}

fn refused<T>(why: impl Into<String>) -> Result<T, BookError> {
    Err(BookError::Refused(why.into()))
}

/// Records `entry` in its own transaction (a savepoint inside the caller's), unless the movement is
/// already booked: then `None` (`INV-17`: one entry per movement; the store refuses a second).
///
/// An entry of fewer than two non-zero postings is never built: nothing is recorded and `None`
/// comes back (review M1; the store would refuse it at commit).
pub async fn record(conn: &mut PgConnection, entry: &Entry) -> sqlx::Result<Option<i64>> {
    let postings: Vec<&Posting> = entry.postings.iter().filter(|p| p.amount > 0).collect();
    if postings.len() < 2 {
        return Ok(None);
    }
    let mut tx = conn.begin().await?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO book_entries (reference, memo, corrects, kind, network, block_number, \
             block_hash, observation_id, transfer_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (kind, reference) WHERE kind IS NOT NULL AND corrects IS NULL DO NOTHING \
         RETURNING id",
    )
    .bind(&entry.reference)
    .bind(&entry.memo)
    .bind(entry.corrects)
    .bind(entry.kind)
    .bind(entry.point.as_ref().map(|p| p.network.as_str()))
    .bind(entry.point.as_ref().map(|p| p.block_number))
    .bind(entry.point.as_ref().map(|p| p.block_hash.as_slice()))
    .bind(entry.observation)
    .bind(entry.transfer)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else {
        tx.rollback().await?;
        return Ok(None);
    };
    for p in postings {
        sqlx::query(
            "INSERT INTO book_postings (entry_id, asset, account, network, side, amount, item) \
             VALUES ($1, $2, $3, $4, $5, $6::numeric, $7)",
        )
        .bind(id)
        .bind(&entry.asset)
        .bind(p.account)
        .bind(&p.network)
        .bind(p.side.as_str())
        .bind(p.amount.to_string())
        .bind(&p.item)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Some(id))
}

// ---------------------------------------------------------------------------------------------
// Posting what the networks show (T-004-01).

/// The final, executed observations that are book movements and are not booked yet: arrivals of
/// an admitted origin token at a reserve address, and the issuing network's and the payouts'
/// events. A movement of zero moves nothing and is never one; a quarantined observation is never
/// tried again.
const PENDING: &str = "SELECT {OBS} FROM observations o \
     JOIN observation_outcomes x ON x.observation_id = o.id \
     WHERE x.outcome = 'final' AND o.executed AND o.amount > 0 \
       AND NOT EXISTS (SELECT 1 FROM book_entries e WHERE e.observation_id = o.id) \
       AND NOT EXISTS (SELECT 1 FROM book_quarantine q WHERE q.observation_id = o.id) \
       AND (o.detail ->> 'event' IN ('Minted', 'Redeemed', 'RefundMinted', 'Payout') \
            OR EXISTS (SELECT 1 FROM reserves r \
                       JOIN admitted_tokens k ON k.network = r.network AND k.asset = r.asset \
                       WHERE r.network = o.network AND r.address = o.recipient \
                         AND k.token = o.token)) \
     ORDER BY o.id";

/// Posts every final movement not yet in the book, each in its own transaction, and returns how
/// many it posted, paging through the service's log ([`post_pending_with`]).
pub async fn post_pending(pool: &PgPool) -> sqlx::Result<usize> {
    Ok(post_pending_with(pool, &LogPager).await?.posted)
}

/// What [`post_pending_with`] did.
#[derive(Clone, Debug, Default)]
pub struct Posted {
    /// Movements posted.
    pub posted: usize,
    /// The assets of the observations it quarantined, one per observation.
    pub quarantined: Vec<String>,
    /// Observations it could not try for a passing failure (the store unreachable); tried again
    /// next time.
    pub deferred: usize,
}

/// Whether the store refused the statement for good (a constraint, a trigger, a value out of
/// range), not for a passing reason (a connection, a serialisation failure, resources, a lock
/// not available: `55P03`, a lock timeout, is tried again, never quarantined).
fn refused_for_good(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(d) => !d.code().is_some_and(|c| {
            c == "55P03"
                || ["08", "40", "53", "57", "58"]
                    .iter()
                    .any(|k| c.starts_with(k))
        }),
        _ => false,
    }
}

/// Posts every final movement not yet in the book, each in its own transaction. A movement that
/// cannot be explained (a mint or a payout no transfer of bezent's ordered) is not posted: the
/// reserve check finds the network and the book apart and pauses (`REQ-BK-1`, `REQ-IC-2`). An
/// observation the store refuses to book is quarantined: recorded once, its asset paused, a human
/// paged through `pager`, and the rest posted as usual (`INV-1`; review M1). Only a failure to read
/// the pending observations at all is an error.
pub async fn post_pending_with(pool: &PgPool, pager: &impl Pager) -> sqlx::Result<Posted> {
    let found: Vec<Observation> = sqlx::query_as(&PENDING.replace("{OBS}", store::OBSERVATION))
        .fetch_all(pool)
        .await?;
    let mut assets: HashMap<String, Option<Asset>> = HashMap::new();
    let mut done = Posted::default();
    for o in found {
        match post_one(pool, &o, &mut assets).await {
            Ok(posted) => done.posted += usize::from(posted),
            Err(e) if refused_for_good(&e) => {
                match quarantine(pool, &o, &e.to_string(), pager).await {
                    Ok(Some(asset)) => done.quarantined.push(asset),
                    Ok(None) => {}
                    Err(q) => {
                        done.deferred += 1;
                        tracing::error!(observation = o.id, error = %e, quarantine = %q,
                            "REQ-IC-2: the book refused an observation that could not be quarantined; tried again next time");
                    }
                }
            }
            Err(e) => {
                done.deferred += 1;
                tracing::warn!(observation = o.id, error = %e, "not posted; tried again next time");
            }
        }
    }
    Ok(done)
}

async fn post_one(
    pool: &PgPool,
    o: &Observation,
    assets: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<bool> {
    let mut conn = pool.acquire().await?;
    let Some(entry) = movement(&mut conn, o, assets).await? else {
        return Ok(false);
    };
    Ok(record(&mut conn, &entry).await?.is_some())
}

/// Sets aside observation `o`, which the store refused to book for `why`: recorded once in
/// `book_quarantine`, its asset's routes paused (`REQ-IC-2`), the event logged, and a human paged.
/// Returns the asset, when one can be told.
async fn quarantine(
    pool: &PgPool,
    o: &Observation,
    why: &str,
    pager: &impl Pager,
) -> sqlx::Result<Option<String>> {
    let mut tx = pool.begin().await?;
    let asset: Option<String> = sqlx::query_scalar(
        "SELECT asset FROM ( \
           SELECT asset, 0 AS rank FROM assets WHERE issuing_asset = $2 \
             AND EXISTS (SELECT 1 FROM networks WHERE network = $1 AND role = 'issuing') \
           UNION ALL SELECT asset, 1 FROM admitted_tokens WHERE network = $1 AND token = $2) a \
         ORDER BY rank LIMIT 1",
    )
    .bind(&o.network)
    .bind(&o.token)
    .fetch_optional(&mut *tx)
    .await?;
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO book_quarantine (observation_id, asset, reason) VALUES ($1, $2, $3) \
         ON CONFLICT (observation_id) DO NOTHING RETURNING id",
    )
    .bind(o.id)
    .bind(&asset)
    .bind(why)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else {
        return Ok(None);
    };
    if let Some(asset) = &asset {
        sqlx::query("INSERT INTO pauses (asset, cause, quarantine_id) VALUES ($1, 'REQ-IC-2', $2)")
            .bind(asset)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    let page = Page {
        kind: "book-quarantine".into(),
        asset: asset.clone().unwrap_or_default(),
        detail: json!({
            "spec": "REQ-IC-2, INV-1",
            "asset": asset,
            "observation": o.id,
            "coordinate": coordinate_key(&o.coordinate()),
            "quarantine": id,
            "reason": why,
            "action": "the observation is set aside and the asset's routes are paused; everything else is still posted and checked",
        }),
    };
    store::event(&mut tx, &page.kind, None, &page.detail).await?;
    tx.commit().await?;
    pager.page(&page);
    Ok(asset)
}

async fn asset<'a>(
    conn: &mut PgConnection,
    cache: &'a mut HashMap<String, Option<Asset>>,
    name: &str,
) -> sqlx::Result<Option<&'a Asset>> {
    if !cache.contains_key(name) {
        let a = Asset::load(conn, name).await?;
        cache.insert(name.to_owned(), a);
    }
    Ok(cache.get(name).and_then(Option::as_ref))
}

fn point_of(o: &Observation) -> FinalPoint {
    FinalPoint {
        network: o.network.clone(),
        block_number: o.block_number,
        block_hash: o.block_hash.clone(),
    }
}

/// The entry a final observation makes, if it is a movement the book can explain.
async fn movement(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<Entry>> {
    let event = o.detail.get("event").and_then(Value::as_str).unwrap_or("");
    let issuing: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM networks WHERE network = $1 AND role = 'issuing')",
    )
    .bind(&o.network)
    .fetch_one(&mut *conn)
    .await?;
    match (issuing, event) {
        (true, "Minted") => mint(conn, o, cache).await,
        (true, "Redeemed") => redemption(conn, o, cache).await,
        (true, "RefundMinted") => refund_mint(conn, o, cache).await,
        (true, _) => Ok(None),
        (false, "Payout") => payout(conn, o, cache).await,
        (false, _) => arrival(conn, o, cache).await,
    }
}

fn unexplained(o: &Observation, why: &str) {
    tracing::warn!(
        observation = o.id,
        network = %o.network,
        block = o.block_number,
        "REQ-BK-1: {why}; not booked, so the reserve check will find the network and the book apart"
    );
}

/// The asset an issuing-network event moves, by its issuing identity.
async fn issued<'a>(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &'a mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<(&'a Asset, u128)>> {
    let name: Option<String> =
        sqlx::query_scalar("SELECT asset FROM assets WHERE issuing_asset = $1")
            .bind(&o.token)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(name) = name else {
        return Ok(None);
    };
    let Some(a) = asset(conn, cache, &name).await? else {
        return Ok(None);
    };
    Ok(a.issuing_ratio()
        .zip(o.amount())
        .and_then(|(r, m)| m.checked_mul(r))
        .map(|book| (a, book)))
}

async fn arrival(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<Entry>> {
    let found: Option<(String, i16)> = sqlx::query_as(
        "SELECT r.asset, k.precision FROM reserves r \
         JOIN admitted_tokens k ON k.network = r.network AND k.asset = r.asset \
         WHERE r.network = $1 AND r.address = $2 AND k.token = $3",
    )
    .bind(&o.network)
    .bind(&o.recipient)
    .bind(&o.token)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((name, precision)) = found else {
        return Ok(None);
    };
    let Some(a) = asset(conn, cache, &name).await? else {
        return Ok(None);
    };
    let Some(amount) = a
        .token_ratio(precision)
        .zip(o.amount())
        .and_then(|(r, m)| m.checked_mul(r))
    else {
        return Ok(None);
    };
    let key = coordinate_key(&o.coordinate());
    // REQ-BK-5: an arrival from an address the operator declared it funds this reserve from, at or
    // after the declaration's block, is the operator's own, from chain data anyone can read; any
    // other arrival is owed mint (§5.3). The store keeps every deposit from binding it (MF1).
    if a.position(&o.network)
        .is_some_and(|p| p.funded_by(o.sender.as_deref(), o.block_number))
    {
        return Ok(Some(Entry {
            kind: "surplus-contribution",
            reference: key.clone(),
            memo: format!(
                "from the declared funding address {}",
                o.sender.as_deref().unwrap_or_default()
            ),
            point: Some(point_of(o)),
            observation: Some(o.id),
            transfer: None,
            corrects: None,
            asset: a.asset.clone(),
            postings: vec![
                Posting::new("reserve", Side::Debit, amount).on(&o.network),
                Posting::new("surplus", Side::Credit, amount).item(&key),
            ],
        }));
    }
    let conversion = o.detail.get("event").and_then(Value::as_str) == Some("Conversion");
    Ok(Some(Entry {
        kind: if conversion { "conversion" } else { "arrival" },
        reference: key.clone(),
        memo: String::new(),
        point: Some(point_of(o)),
        observation: Some(o.id),
        transfer: None,
        corrects: None,
        asset: a.asset.clone(),
        postings: vec![
            Posting::new("reserve", Side::Debit, amount).on(&o.network),
            Posting::new("owed-mint", Side::Credit, amount).item(&key),
        ],
    }))
}

/// A transfer with its source binding, as the book reads it.
#[derive(sqlx::FromRow)]
struct Bound {
    id: i64,
    bridge_reference: String,
    source_network: Option<String>,
    source_block_number: Option<i64>,
    source_block_hash: Option<Vec<u8>>,
    source_position_kind: Option<String>,
    source_position_index: Option<i64>,
}

const BOUND: &str = "id, bridge_reference, source_network, source_block_number, \
    source_block_hash, source_position_kind::text AS source_position_kind, source_position_index";

impl Bound {
    fn source(&self) -> Option<Coordinate> {
        Some(Coordinate {
            network: self.source_network.clone()?,
            block_number: self.source_block_number?,
            block_hash: self.source_block_hash.clone()?,
            position_kind: self.source_position_kind.clone()?,
            position_index: self.source_position_index?,
        })
    }
}

/// The deposit a mint reference belongs to, with the arrival it binds.
async fn deposit_of(
    conn: &mut PgConnection,
    reference: &[u8],
) -> sqlx::Result<Option<(i64, String, Coordinate)>> {
    let row: Option<Bound> = sqlx::query_as(&format!(
        "SELECT {BOUND} FROM transfers WHERE kind = 'deposit' AND on_chain_reference = $1"
    ))
    .bind(reference)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.and_then(|b| {
        let source = b.source()?;
        Some((b.id, b.bridge_reference, source))
    }))
}

async fn mint(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<Entry>> {
    let Some(reference) = o.on_chain_reference.as_deref() else {
        return Ok(None);
    };
    let Some((deposit, bridge, source)) = deposit_of(conn, reference).await? else {
        unexplained(o, "a mint no deposit of bezent's carries the reference of");
        return Ok(None);
    };
    let Some(arrived) = store::observation_at(conn, &source).await? else {
        unexplained(o, "a mint whose deposit's arrival is not observed");
        return Ok(None);
    };
    let precision: Option<i16> = sqlx::query_scalar(
        "SELECT precision FROM admitted_tokens WHERE network = $1 AND token = $2",
    )
    .bind(&arrived.network)
    .bind(&arrived.token)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((a, minted)) = issued(conn, o, cache).await? else {
        return Ok(None);
    };
    let (Some(ratio), Some(owed)) = (
        a.issuing_ratio(),
        precision
            .and_then(|p| a.token_ratio(p))
            .zip(arrived.amount())
            .and_then(|(r, m)| m.checked_mul(r)),
    ) else {
        return Ok(None);
    };
    let item = coordinate_key(&source);
    let mut postings = vec![Posting::new("supply", Side::Credit, minted)];
    match owed.checked_sub(minted) {
        // REQ-AS-9: the arrival is owed in full and leaves owed mint in full; what is below one
        // minor unit of the asset is surplus, with the deposit's reference.
        Some(remainder) if remainder < ratio => {
            postings.push(Posting::new("owed-mint", Side::Debit, owed).item(&item));
            postings.push(Posting::new("surplus", Side::Credit, remainder).item(&bridge));
        }
        // A mint of another amount than the deposit's (the core sends it to review, REQ-TR-5):
        // the book records what was minted against the arrival and keeps the rest owed, so the
        // check finds the item apart from the network's and pauses.
        _ => postings.push(Posting::new("owed-mint", Side::Debit, minted).item(&item)),
    }
    Ok(Some(Entry {
        kind: "mint",
        reference: coordinate_key(&o.coordinate()),
        memo: format!("deposit {bridge}"),
        point: Some(point_of(o)),
        observation: Some(o.id),
        transfer: Some(deposit),
        corrects: None,
        asset: a.asset.clone(),
        postings,
    }))
}

async fn redemption(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<Entry>> {
    let (Some(redeemer), Some(reference)) = (o.sender.as_deref(), o.on_chain_reference.as_deref())
    else {
        return Ok(None);
    };
    let item = refs::withdrawal(redeemer, reference);
    let Some((a, amount)) = issued(conn, o, cache).await? else {
        return Ok(None);
    };
    Ok(Some(Entry {
        kind: "redemption",
        reference: coordinate_key(&o.coordinate()),
        memo: String::new(),
        point: Some(point_of(o)),
        observation: Some(o.id),
        transfer: None,
        corrects: None,
        asset: a.asset.clone(),
        postings: vec![
            Posting::new("supply", Side::Debit, amount),
            Posting::new("owed-payout", Side::Credit, amount).item(&item),
        ],
    }))
}

async fn refund_mint(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<Entry>> {
    // A refund mint is to the redeemer, under the redemption's reference (amendment 0008): the
    // withdrawal it closes is (redeemer, reference).
    let (Some(redeemer), Some(reference)) =
        (o.recipient.as_deref(), o.on_chain_reference.as_deref())
    else {
        return Ok(None);
    };
    let item = refs::withdrawal(redeemer, reference);
    let Some((a, amount)) = issued(conn, o, cache).await? else {
        return Ok(None);
    };
    Ok(Some(Entry {
        kind: "refund-mint",
        reference: coordinate_key(&o.coordinate()),
        memo: String::new(),
        point: Some(point_of(o)),
        observation: Some(o.id),
        transfer: None,
        corrects: None,
        asset: a.asset.clone(),
        postings: vec![
            Posting::new("owed-payout", Side::Debit, amount).item(&item),
            Posting::new("supply", Side::Credit, amount),
        ],
    }))
}

async fn payout(
    conn: &mut PgConnection,
    o: &Observation,
    cache: &mut HashMap<String, Option<Asset>>,
) -> sqlx::Result<Option<Entry>> {
    let Some(reference) = o.on_chain_reference.as_deref() else {
        return Ok(None);
    };
    // A withdrawal's payout carries the withdrawal's own reference (review M1); the refund of an
    // unassociated arrival carries one derived from the arrival.
    let withdrawal: Option<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT id, bridge_reference, asset FROM transfers \
         WHERE kind = 'withdrawal' AND destination_reference = $1",
    )
    .bind(reference)
    .fetch_optional(&mut *conn)
    .await?;
    let refund: Option<Bound> = if withdrawal.is_none() {
        sqlx::query_as(&format!(
            "SELECT {BOUND} FROM transfers WHERE kind = 'refund' AND refund_of IS NULL \
               AND on_chain_reference = $1"
        ))
        .bind(reference)
        .fetch_optional(&mut *conn)
        .await?
    } else {
        None
    };
    let (kind, transfer, account, item, name) = match (
        withdrawal,
        refund.as_ref().and_then(|b| Some((b.id, b.source()?))),
    ) {
        (Some((id, bridge, Some(asset))), _) => ("payout", id, "owed-payout", bridge, asset),
        (None, Some((id, source))) => {
            let name: Option<String> = sqlx::query_scalar(
                "SELECT asset FROM admitted_tokens WHERE network = $1 AND token = $2",
            )
            .bind(&o.network)
            .bind(&o.token)
            .fetch_optional(&mut *conn)
            .await?;
            let Some(name) = name else {
                unexplained(o, "a refund payout of a token admitted for no asset");
                return Ok(None);
            };
            (
                "unassociated-refund",
                id,
                "owed-mint",
                coordinate_key(&source),
                name,
            )
        }
        _ => {
            unexplained(
                o,
                "a payout no transfer of bezent's carries the reference of",
            );
            return Ok(None);
        }
    };
    let Some(a) = asset(conn, cache, &name).await? else {
        return Ok(None);
    };
    let Some(position) = a.position(&o.network) else {
        unexplained(o, "a payout on a network where the asset holds no reserve");
        return Ok(None);
    };
    if !o
        .sender
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case(&position.address))
    {
        unexplained(o, "a payout that did not leave the reserve address");
        return Ok(None);
    }
    let precision: Option<i16> = sqlx::query_scalar(
        "SELECT precision FROM admitted_tokens WHERE network = $1 AND token = $2 AND asset = $3",
    )
    .bind(&o.network)
    .bind(&o.token)
    .bind(&a.asset)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(amount) = precision
        .and_then(|p| a.token_ratio(p))
        .zip(o.amount())
        .and_then(|(r, m)| m.checked_mul(r))
    else {
        unexplained(o, "a payout of a token not admitted for its asset");
        return Ok(None);
    };
    Ok(Some(Entry {
        kind,
        reference: coordinate_key(&o.coordinate()),
        memo: String::new(),
        point: Some(point_of(o)),
        observation: Some(o.id),
        transfer: Some(transfer),
        corrects: None,
        asset: a.asset.clone(),
        postings: vec![
            Posting::new(account, Side::Debit, amount).item(&item),
            Posting::new("reserve", Side::Credit, amount).on(&o.network),
        ],
    }))
}

// ---------------------------------------------------------------------------------------------
// Movements recorded by declaration or report (REQ-BK-4, REQ-BK-5, REQ-BE-6).

async fn load(conn: &mut PgConnection, name: &str) -> Result<Asset, BookError> {
    match Asset::load(conn, name).await? {
        Some(a) => Ok(a),
        None => refused(format!("{name} is not a declared asset")),
    }
}

/// A final, executed observation at a reserve address of `asset`, with its amount in book units:
/// `Some(sender side)` when it left the reserve, `None` when it arrived.
async fn reserve_movement(
    conn: &mut PgConnection,
    a: &Asset,
    observation: i64,
) -> Result<(Observation, u128, bool), BookError> {
    let o: Option<Observation> = sqlx::query_as(&format!(
        "SELECT {} FROM observations o JOIN observation_outcomes x ON x.observation_id = o.id \
         WHERE o.id = $1 AND x.outcome = 'final' AND o.executed",
        store::OBSERVATION
    ))
    .bind(observation)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(o) = o else {
        return refused("not a final, executed observation (INV-16)");
    };
    let Some(position) = a.position(&o.network) else {
        return refused(format!("{} holds no reserve on {}", a.asset, o.network));
    };
    let Some((token, precision)) = &position.token else {
        return refused("no origin token is admitted there (REQ-AS-2)");
    };
    if o.token.as_deref() != Some(token.as_str()) {
        return refused("not the asset's origin token (REQ-AS-7: a convertible backs nothing)");
    }
    let outflow = o
        .sender
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case(&position.address));
    let inflow = o
        .recipient
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case(&position.address));
    if outflow == inflow {
        return refused("not a movement into or out of the reserve address (REQ-BK-5)");
    }
    if o.amount() == Some(0) {
        return refused("a movement of zero moves nothing");
    }
    let Some(amount) = a
        .token_ratio(*precision)
        .zip(o.amount())
        .and_then(|(r, m)| m.checked_mul(r))
    else {
        return refused("amount out of range (INV-11)");
    };
    Ok((o, amount, outflow))
}

/// Records the final outflow `outflow` from a reserve address as a withdrawal of the operator's
/// surplus (`REQ-BK-5`, `INV-14`).
pub async fn surplus_withdrawal(
    conn: &mut PgConnection,
    asset: &str,
    outflow: i64,
    reference: &str,
) -> Result<i64, BookError> {
    let a = load(conn, asset).await?;
    let (o, amount, out) = reserve_movement(conn, &a, outflow).await?;
    if !out {
        return refused("a surplus withdrawal leaves the reserve address");
    }
    if o.detail.get("event").and_then(Value::as_str) == Some("Payout") {
        return refused("a payout is a transfer's, never surplus (REQ-BK-5)");
    }
    let booked: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM book_entries WHERE observation_id = $1)")
            .bind(o.id)
            .fetch_one(&mut *conn)
            .await?;
    if booked {
        return refused("the movement is already booked (INV-17)");
    }
    match record(
        conn,
        &Entry {
            kind: "surplus-withdrawal",
            reference: reference.to_owned(),
            memo: coordinate_key(&o.coordinate()),
            point: Some(point_of(&o)),
            observation: Some(o.id),
            transfer: None,
            corrects: None,
            asset: a.asset.clone(),
            postings: vec![
                Posting::new("surplus", Side::Debit, amount),
                Posting::new("reserve", Side::Credit, amount).on(&o.network),
            ],
        },
    )
    .await?
    {
        Some(id) => Ok(id),
        None => refused(format!("{reference} is already recorded (INV-17)")),
    }
}

/// What a cost is (`REQ-BK-4`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cost {
    /// A network cost paid from a reserve position.
    Network,
    /// A venue fee, or an amount difference on a venue delivery.
    Venue,
}

/// Records a cost paid from the reserve position on `network`, against surplus, with its
/// reference and, when observed, the final point it rests on (`REQ-BK-4`, `AC-E2.2`). No delivery
/// is reduced by it (`REQ-TR-5`): the book takes it from the operator's surplus.
pub async fn cost(
    conn: &mut PgConnection,
    asset: &str,
    kind: Cost,
    network: &str,
    amount: u128,
    reference: &str,
    point: Option<FinalPoint>,
) -> Result<i64, BookError> {
    let a = load(conn, asset).await?;
    if a.position(network).is_none() {
        return refused(format!("{asset} holds no reserve on {network}"));
    }
    if amount == 0 {
        return refused("a cost of zero");
    }
    match record(
        conn,
        &Entry {
            kind: match kind {
                Cost::Network => "network-cost",
                Cost::Venue => "venue-fee",
            },
            reference: reference.to_owned(),
            memo: String::new(),
            point,
            observation: None,
            transfer: None,
            corrects: None,
            asset: a.asset.clone(),
            postings: vec![
                Posting::new("surplus", Side::Debit, amount),
                Posting::new("reserve", Side::Credit, amount).on(network),
            ],
        },
    )
    .await?
    {
        Some(id) => Ok(id),
        None => refused(format!("{reference} is already recorded (INV-17)")),
    }
}

/// An attesting party's report (`REQ-BE-4`: recorded with the party's own identifiers).
#[derive(Clone, Debug)]
pub struct Report<'a> {
    /// The attesting party.
    pub party: &'a str,
    /// The report's identifier, the party's own.
    pub report: &'a str,
}

impl Report<'_> {
    fn reference(&self) -> String {
        format!("{}:{}", self.party, self.report)
    }
}

/// Records the attesting party's report that a payment it reported final was reversed
/// (`REQ-BE-6`): the reserve at `position` on `network` falls by `amount` and the shortfall is
/// charged against surplus. Holders' balances are untouched and nothing minted is clawed back.
/// If surplus falls below its floor, new mints of the asset pause until the reserve is restored
/// ([`mints_paused`]). Returns the entry; the reversal's reference is the report's.
pub async fn reversal(
    conn: &mut PgConnection,
    asset: &str,
    network: &str,
    position: &str,
    amount: u128,
    report: &Report<'_>,
) -> Result<i64, BookError> {
    let a = load(conn, asset).await?;
    let Some(p) = a.position(network) else {
        return refused(format!("{asset} holds no reserve on {network}"));
    };
    if !p.attested() {
        return refused("a reversal is an attested backend's (REQ-BE-6)");
    }
    if amount == 0 {
        return refused("a reversal of zero");
    }
    match record(
        conn,
        &Entry {
            kind: "reversal",
            reference: report.reference(),
            memo: format!("reversal reported by {}", report.party),
            point: None,
            observation: None,
            transfer: None,
            corrects: None,
            asset: a.asset.clone(),
            postings: vec![
                Posting::new("surplus", Side::Debit, amount),
                Posting::new("reserve", Side::Credit, amount)
                    .on(network)
                    .item(position),
            ],
        },
    )
    .await?
    {
        Some(id) => Ok(id),
        None => refused("the report is already recorded (INV-17)"),
    }
}

/// Records the attesting party's report that it restored the reserve a reversal took
/// (`REQ-BE-6`), in full, at the same position.
pub async fn restoration(
    conn: &mut PgConnection,
    asset: &str,
    reversal: &Report<'_>,
    report: &Report<'_>,
) -> Result<i64, BookError> {
    let a = load(conn, asset).await?;
    let found: Option<(i64, Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT e.id, p.network, p.item, p.amount::text FROM book_entries e \
         JOIN book_postings p ON p.entry_id = e.id AND p.account = 'reserve' \
         WHERE e.kind = 'reversal' AND e.reference = $1 AND p.asset = $2",
    )
    .bind(reversal.reference())
    .bind(&a.asset)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((_, Some(network), item, amount)) = found else {
        return refused("no such reversal of this asset");
    };
    // REQ-BE-6: the attesting party that reported the reversal restores it (review S8).
    if report.party != reversal.party {
        return refused("only the party that reported the reversal restores it (REQ-BE-6)");
    }
    let Ok(amount) = amount.parse::<u128>() else {
        return refused("amount out of range (INV-11)");
    };
    let mut credit = Posting::new("reserve", Side::Debit, amount).on(&network);
    credit.item = item;
    match record(
        conn,
        &Entry {
            kind: "restoration",
            reference: reversal.reference(),
            memo: format!(
                "restored, reported by {} as {}",
                report.party, report.report
            ),
            point: None,
            observation: None,
            transfer: None,
            corrects: None,
            asset: a.asset.clone(),
            postings: vec![credit, Posting::new("surplus", Side::Credit, amount)],
        },
    )
    .await?
    {
        Some(id) => Ok(id),
        None => refused("the reversal is already restored (INV-17)"),
    }
}

/// Records a correction of entry `corrects`: a new entry, never a change (`INV-17`). Its postings
/// must balance; the store refuses one that does not.
pub async fn correction(
    conn: &mut PgConnection,
    corrects: i64,
    asset: &str,
    memo: &str,
    postings: Vec<Posting>,
) -> Result<i64, BookError> {
    let a = load(conn, asset).await?;
    match record(
        conn,
        &Entry {
            kind: "correction",
            reference: format!("correction-of-{corrects}"),
            memo: memo.to_owned(),
            point: None,
            observation: None,
            transfer: None,
            corrects: Some(corrects),
            asset: a.asset.clone(),
            postings,
        },
    )
    .await?
    {
        Some(id) => Ok(id),
        None => refused("not recorded"),
    }
}

/// Builds a posting for [`correction`].
pub fn posting(
    account: &'static str,
    network: Option<&str>,
    side: Side,
    amount: u128,
    item: Option<&str>,
) -> Posting {
    Posting {
        account,
        network: network.map(str::to_owned),
        side,
        amount,
        item: item.map(str::to_owned),
    }
}

/// Whether new mints of `asset` are paused under `REQ-BE-6`: a reversal took its surplus below the
/// floor and the reserve is not restored yet.
pub async fn mints_paused(conn: &mut PgConnection, asset: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS ( \
           SELECT 1 FROM book_entries r \
           WHERE r.kind = 'reversal' \
             AND EXISTS (SELECT 1 FROM book_postings p WHERE p.entry_id = r.id AND p.asset = $1) \
             AND NOT EXISTS (SELECT 1 FROM book_entries s \
                             WHERE s.kind = 'restoration' AND s.reference = r.reference) \
             AND (SELECT COALESCE(sum(CASE p.side WHEN 'credit' THEN p.amount ELSE -p.amount END), 0) \
                  FROM book_postings p \
                  WHERE p.asset = $1 AND p.account = 'surplus' AND p.entry_id <= r.id) \
                 < COALESCE((SELECT surplus_floor FROM assets WHERE asset = $1), 0))",
    )
    .bind(asset)
    .fetch_one(conn)
    .await
}

/// The reversals of `asset` not restored, with when each was recorded (`REQ-BE-6`).
pub async fn unrestored_reversals(
    conn: &mut PgConnection,
    asset: &str,
) -> sqlx::Result<Vec<(String, chrono::DateTime<chrono::Utc>)>> {
    sqlx::query_as(
        "SELECT r.reference, r.recorded_at FROM book_entries r \
         WHERE r.kind = 'reversal' \
           AND EXISTS (SELECT 1 FROM book_postings p WHERE p.entry_id = r.id AND p.asset = $1) \
           AND NOT EXISTS (SELECT 1 FROM book_entries s \
                           WHERE s.kind = 'restoration' AND s.reference = r.reference) \
         ORDER BY r.id",
    )
    .bind(asset)
    .fetch_all(conn)
    .await
}

/// Every movement of `asset`'s surplus the book holds as of `cuts`, in the order recorded, with
/// its kind, its reference, the point it rests on and its amount (positive into surplus): the
/// contributions, withdrawals, network costs and venue fees, `REQ-AS-9` remainders, reversals,
/// restorations and corrections, so a statement shows how the surplus came to be (`REQ-BK-4`,
/// `REQ-BK-5`; PLAN `DEC-16`; review S2). With `entry_cut`, the evaluation's, no entry whose
/// transaction had not committed when the evaluation read the book is listed, whatever its id and
/// whatever block it rests on, so the movements add up to the surplus the evaluation's book holds
/// (review SF5; issue #42: cut by commit order, not by id).
pub async fn surplus_movements(
    conn: &mut PgConnection,
    asset: &str,
    cuts: &BTreeMap<String, i64>,
    entry_cut: Option<&EntryCut>,
) -> sqlx::Result<Vec<Value>> {
    let networks: Vec<&str> = cuts.keys().map(String::as_str).collect();
    let blocks: Vec<i64> = cuts.values().copied().collect();
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<i64>,
        String,
    )> = sqlx::query_as(
        "SELECT e.id, e.kind, e.reference, e.memo, e.network, e.block_number, \
                 sum(CASE p.side WHEN 'credit' THEN p.amount ELSE -p.amount END)::text \
             FROM book_postings p JOIN book_entries e ON e.id = p.entry_id \
             WHERE p.asset = $1 AND p.account = 'surplus' \
               AND ($4::bigint IS NULL OR e.id <= $4) \
               AND ($5::text IS NULL \
                    OR pg_visible_in_snapshot(e.recorded_tx::text::xid8, $5::text::pg_snapshot)) \
               AND (e.network IS NULL OR e.block_number <= \
                    (SELECT c.cut FROM unnest($2::text[], $3::bigint[]) AS c(network, cut) \
                     WHERE c.network = e.network)) \
             GROUP BY e.id ORDER BY e.id",
    )
    .bind(asset)
    .bind(&networks)
    .bind(&blocks)
    .bind(entry_cut.map(|c| c.id))
    .bind(entry_cut.and_then(|c| c.snapshot.as_deref()))
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, reference, memo, network, block, amount)| {
            json!({"entry": id, "kind": kind, "reference": reference, "amount": amount,
                   "network": network, "block_number": block, "memo": memo})
        })
        .collect())
}

// ---------------------------------------------------------------------------------------------
// The book as of final points (REQ-BK-8).

/// The book as an evaluation read it (review SF5; issue #42): the entries whose transactions had
/// committed when it read, by the snapshot it read under, and the last id among them. An entry
/// with a lower id whose transaction commits later is not in the cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntryCut {
    /// The last entry id visible.
    pub id: i64,
    /// The snapshot (`pg_snapshot`, as text) the book was read under; `None` for an evaluation
    /// recorded before migration 0039, which is cut by id alone.
    pub snapshot: Option<String>,
}

/// The book now, as an evaluation records the book it read: the snapshot of this statement and the
/// last entry id it sees (both from one statement, so under one snapshot).
pub async fn entry_cut(conn: &mut PgConnection) -> sqlx::Result<EntryCut> {
    let (id, snapshot): (i64, String) = sqlx::query_as(
        "SELECT COALESCE(max(id), 0), pg_current_snapshot()::text FROM book_entries",
    )
    .fetch_one(conn)
    .await?;
    Ok(EntryCut {
        id,
        snapshot: Some(snapshot),
    })
}

/// The book's terms for `asset` as of `cuts`, one final block number per network: every entry
/// that rests on a block at or before its network's cut, and every entry that rests on no block
/// (a report or a declaration). An entry resting on a network not in `cuts` is left out, and so,
/// with `entry_cut`, is every entry whose transaction had not committed when the cut was taken
/// (review SF5; issue #42).
pub async fn as_of(
    conn: &mut PgConnection,
    asset: &str,
    cuts: &BTreeMap<String, i64>,
    entry_cut: Option<&EntryCut>,
) -> sqlx::Result<Terms> {
    let networks: Vec<&str> = cuts.keys().map(String::as_str).collect();
    let blocks: Vec<i64> = cuts.values().copied().collect();
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT p.account, COALESCE(p.network, ''), COALESCE(p.item, ''), \
             sum(CASE p.side WHEN 'debit' THEN p.amount ELSE -p.amount END)::text \
         FROM book_postings p JOIN book_entries e ON e.id = p.entry_id \
         WHERE p.asset = $1 \
           AND ($4::bigint IS NULL OR e.id <= $4) \
           AND ($5::text IS NULL \
                OR pg_visible_in_snapshot(e.recorded_tx::text::xid8, $5::text::pg_snapshot)) \
           AND (e.network IS NULL OR e.block_number <= \
                (SELECT c.cut FROM unnest($2::text[], $3::bigint[]) AS c(network, cut) \
                 WHERE c.network = e.network)) \
         GROUP BY 1, 2, 3",
    )
    .bind(asset)
    .bind(&networks)
    .bind(&blocks)
    .bind(entry_cut.map(|c| c.id))
    .bind(entry_cut.and_then(|c| c.snapshot.as_deref()))
    .fetch_all(conn)
    .await?;
    let mut t = Terms::default();
    for (account, network, item, balance) in rows {
        // Debits minus credits; credit-normal accounts are read the other way.
        let debit: i128 = balance.parse().unwrap_or(0);
        let item = if item.is_empty() {
            UNITEMISED.to_owned()
        } else {
            item
        };
        match account.as_str() {
            "reserve" => *t.reserve.entry(network).or_default() += debit,
            "in-transit" => *t.in_transit.entry(item).or_default() += debit,
            "supply" => t.supply -= debit,
            "owed-mint" => *t.owed_mint.entry(item).or_default() -= debit,
            "owed-payout" => *t.owed_payout.entry(item).or_default() -= debit,
            "surplus" => t.surplus -= debit,
            _ => {}
        }
    }
    // A closed item is no item (REQ-BK-2: the list of what makes the term up).
    for m in [&mut t.in_transit, &mut t.owed_mint, &mut t.owed_payout] {
        m.retain(|_, v| *v != 0);
    }
    Ok(t)
}
