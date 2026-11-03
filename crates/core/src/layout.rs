use serde::{Deserialize, Serialize};

/// Edges no farther apart than this still count as one crossing, and a Windows refresh
/// closes a crack of this width so a glued computer stays glued.
pub const ADJACENT_GAP_PX: i32 = 32;

/// A dragged computer whose edge comes this close to another edge is glued flush to it.
pub const SNAP_PX: i32 = 72;

/// One physical display belonging to a host (Monitor Manager tile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorTile {
    pub id: String,
    pub host: String,
    pub label: String,
    /// Top-left in virtual layout space (usually OS virtual-desktop pixels).
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Primary display according to the OS.
    #[serde(default)]
    pub primary: bool,
    /// Geometry comes from the OS — refresh overwrites x/y/w/h; don't free-drag.
    #[serde(default)]
    pub system_synced: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MonitorLayout {
    pub monitors: Vec<MonitorTile>,
}

impl MonitorLayout {
    /// Starter layout so Monitor Manager isn't empty on first run.
    pub fn demo_local() -> Self {
        let host = hostname_or("THIS-PC");
        Self {
            monitors: vec![
                MonitorTile {
                    id: "local-a".into(),
                    host: host.clone(),
                    label: "A".into(),
                    x: 0,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    primary: true,
                    system_synced: false,
                },
                MonitorTile {
                    id: "local-b".into(),
                    host,
                    label: "B".into(),
                    x: 1920,
                    y: 0,
                    width: 1920,
                    height: 1080,
                    primary: false,
                    system_synced: false,
                },
            ],
        }
    }

    /// Replace this PC's tiles with `system`, keep remote-host tiles.
    pub fn merge_system_displays(&mut self, system: MonitorLayout) {
        let host = system
            .monitors
            .first()
            .map(|m| m.host.clone())
            .unwrap_or_else(|| hostname_or("THIS-PC"));
        self.monitors
            .retain(|m| !m.host.eq_ignore_ascii_case(&host));
        self.monitors.extend(system.monitors);
        self.monitors.sort_by(|a, b| {
            a.y.cmp(&b.y)
                .then(a.x.cmp(&b.x))
                .then(a.label.cmp(&b.label))
        });
        // Local tiles just jumped to the current Windows pixels. Pull a computer that was
        // already sitting against them back across a crack, without moving one that was
        // deliberately left apart.
        let mut remotes = Vec::new();
        for m in &self.monitors {
            if m.host.eq_ignore_ascii_case(&host) {
                continue;
            }
            if remotes
                .iter()
                .any(|h: &String| h.eq_ignore_ascii_case(&m.host))
            {
                continue;
            }
            remotes.push(m.host.clone());
        }
        for h in remotes {
            self.snap_host(&h);
        }
    }

    /// Slide every display of `host` by the same delta. The Windows arrangement inside the
    /// computer stays put. This computer's own displays (they come from Windows and the cursor
    /// is matched against those pixels) do not move.
    pub fn translate_host(&mut self, host: &str, dx: i32, dy: i32) -> bool {
        if dx == 0 && dy == 0 || self.host_is_pinned(host) {
            return false;
        }
        let mut moved = false;
        for m in &mut self.monitors {
            if m.host.eq_ignore_ascii_case(host) {
                m.x = m.x.saturating_add(dx);
                m.y = m.y.saturating_add(dy);
                moved = true;
            }
        }
        moved
    }

    /// Glue `host` onto the nearest other edge. The reach is half the shorter side, so a gap
    /// that still looks like "next to this screen" on the canvas closes, and a computer left
    /// a full screen away stays there. Only the gap closes: the position along the edge stays
    /// where the user dropped it (no pull to a corner). Already-flush edges are left alone.
    /// This computer's own displays do not move.
    pub fn snap_host(&mut self, host: &str) -> bool {
        if self.host_is_pinned(host) {
            return false;
        }
        let ours: Vec<MonitorTile> = self
            .monitors
            .iter()
            .filter(|m| m.host.eq_ignore_ascii_case(host))
            .cloned()
            .collect();
        if ours.is_empty() {
            return false;
        }
        let others: Vec<MonitorTile> = self
            .monitors
            .iter()
            .filter(|m| !m.host.eq_ignore_ascii_case(host))
            .cloned()
            .collect();
        // (perpendicular distance, dx, dy)
        let mut best: Option<(i32, i32, i32)> = None;
        let mut consider = |sep: i32, dx: i32, dy: i32, reach: i32| {
            let dist = sep.abs();
            if dist == 0 || dist > reach {
                return;
            }
            if best.map(|(d, _, _)| dist < d).unwrap_or(true) {
                best = Some((dist, dx, dy));
            }
        };
        for a in &ours {
            let ar = a.x + a.width as i32;
            let ab = a.y + a.height as i32;
            for b in &others {
                let br = b.x + b.width as i32;
                let bb = b.y + b.height as i32;
                if y_ranges_overlap(a, b) {
                    let reach = glue_reach(a.height, b.height);
                    consider(b.x - ar, b.x - ar, 0, reach);
                    consider(a.x - br, br - a.x, 0, reach);
                }
                if x_ranges_overlap(a, b) {
                    let reach = glue_reach(a.width, b.width);
                    consider(b.y - ab, 0, b.y - ab, reach);
                    consider(a.y - bb, 0, bb - a.y, reach);
                }
            }
        }
        best.is_some_and(|(_, dx, dy)| self.translate_host(host, dx, dy))
    }

    fn host_is_pinned(&self, host: &str) -> bool {
        self.monitors
            .iter()
            .any(|m| m.host.eq_ignore_ascii_case(host) && m.system_synced)
    }

    /// Find which remote monitor (if any) is adjacent when leaving `from` across an edge.
    pub fn neighbor_across(
        &self,
        from_id: &str,
        edge: inputshare_protocol::Edge,
        along: f32,
    ) -> Option<EdgeHit> {
        let from = self.monitors.iter().find(|m| m.id == from_id)?;
        let along = along.clamp(0.0, 1.0);

        let (probe_x, probe_y) = match edge {
            inputshare_protocol::Edge::Left => {
                (from.x - 1, from.y + ((from.height as f32) * along) as i32)
            }
            inputshare_protocol::Edge::Right => (
                from.x + from.width as i32,
                from.y + ((from.height as f32) * along) as i32,
            ),
            inputshare_protocol::Edge::Top => {
                (from.x + ((from.width as f32) * along) as i32, from.y - 1)
            }
            inputshare_protocol::Edge::Bottom => (
                from.x + ((from.width as f32) * along) as i32,
                from.y + from.height as i32,
            ),
        };

        for m in &self.monitors {
            if m.id == from.id {
                continue;
            }
            if point_in(m, probe_x, probe_y) {
                return Some(edge_hit(edge, m, probe_x, probe_y));
            }
        }
        // A crack of a few pixels (the old 16px drag grid left these) is still one edge.
        self.neighbor_across_gap(from, edge, along)
    }

    /// Nearest display just past `edge`, within [`ADJACENT_GAP_PX`], that contains the
    /// along-edge point. `None` when the only candidates overlap this display or sit farther.
    fn neighbor_across_gap(
        &self,
        from: &MonitorTile,
        edge: inputshare_protocol::Edge,
        along: f32,
    ) -> Option<EdgeHit> {
        use inputshare_protocol::Edge::*;
        let along = along.clamp(0.0, 1.0);
        let along_x = from.x + ((from.width as f32) * along) as i32;
        let along_y = from.y + ((from.height as f32) * along) as i32;
        let mut best: Option<(&MonitorTile, i32)> = None;
        for m in &self.monitors {
            if m.id == from.id {
                continue;
            }
            let sep = match edge {
                Right => m.x - (from.x + from.width as i32),
                Left => from.x - (m.x + m.width as i32),
                Bottom => m.y - (from.y + from.height as i32),
                Top => from.y - (m.y + m.height as i32),
            };
            if sep <= 0 || sep > ADJACENT_GAP_PX {
                continue;
            }
            let along_ok = match edge {
                Left | Right => (m.y..m.y + m.height as i32).contains(&along_y),
                Top | Bottom => (m.x..m.x + m.width as i32).contains(&along_x),
            };
            if !along_ok {
                continue;
            }
            if best.map(|(_, d)| sep < d).unwrap_or(true) {
                best = Some((m, sep));
            }
        }
        let (m, _) = best?;
        let (probe_x, probe_y) = match edge {
            Right => (m.x, along_y),
            Left => (m.x + m.width as i32 - 1, along_y),
            Bottom => (along_x, m.y),
            Top => (along_x, m.y + m.height as i32 - 1),
        };
        Some(edge_hit(edge, m, probe_x, probe_y))
    }

    /// A display whose edge faces `from` and overlaps it along that edge, even when the cursor
    /// is past the end of the shorter screen. `entry` is the fraction along the source edge,
    /// so the shorter screen is scaled to the whole side (ShareMouse "equalize").
    pub fn neighbor_beside(
        &self,
        from_id: &str,
        edge: inputshare_protocol::Edge,
        source_along: f32,
    ) -> Option<EdgeHit> {
        use inputshare_protocol::Edge::*;
        let from = self.monitors.iter().find(|m| m.id == from_id)?;
        let mut best: Option<(&MonitorTile, i32)> = None;
        for m in &self.monitors {
            if m.id == from.id {
                continue;
            }
            let sep = match edge {
                Right => m.x - (from.x + from.width as i32),
                Left => from.x - (m.x + m.width as i32),
                Bottom => m.y - (from.y + from.height as i32),
                Top => from.y - (m.y + m.height as i32),
            };
            if !(0..=ADJACENT_GAP_PX).contains(&sep) {
                continue;
            }
            let facing = match edge {
                Left | Right => y_ranges_overlap(from, m),
                Top | Bottom => x_ranges_overlap(from, m),
            };
            if !facing {
                continue;
            }
            if best.map(|(_, d)| sep < d).unwrap_or(true) {
                best = Some((m, sep));
            }
        }
        let (m, _) = best?;
        Some(EdgeHit {
            monitor_id: m.id.clone(),
            host: m.host.clone(),
            entry_edge: opposite(edge),
            entry: source_along.clamp(0.0, 1.0),
        })
    }

    /// Like [`neighbor_across`], but if nothing is adjacent and `wrap` is on,
    /// jump using axis-aware wrap (same X for vertical, same Y for horizontal).
    pub fn neighbor_or_wrap(
        &self,
        from_id: &str,
        edge: inputshare_protocol::Edge,
        along: f32,
        wrap: bool,
    ) -> Option<EdgeHit> {
        if let Some(hit) = self.neighbor_across(from_id, edge, along) {
            return Some(hit);
        }
        if !wrap {
            return None;
        }
        self.wrap_across(from_id, edge, along)
    }

    /// Axis-aware wrap:
    /// - Up/down: keep X, pick monitor that contains that X on the opposite extreme
    ///   (A↑ → bottom of B under the same X).
    /// - Left/right: keep Y, pick monitor that contains that Y on the opposite extreme
    ///   (D← → right of C, C→ → left of D).
    pub fn wrap_across(
        &self,
        from_id: &str,
        edge: inputshare_protocol::Edge,
        along: f32,
    ) -> Option<EdgeHit> {
        let from = self.monitors.iter().find(|m| m.id == from_id)?;
        let along = along.clamp(0.0, 1.0);
        let (cx, cy) = match edge {
            inputshare_protocol::Edge::Left | inputshare_protocol::Edge::Right => (
                from.x
                    + if matches!(edge, inputshare_protocol::Edge::Right) {
                        from.width.saturating_sub(1) as i32
                    } else {
                        0
                    },
                from.y + ((from.height as f32) * along) as i32,
            ),
            inputshare_protocol::Edge::Top | inputshare_protocol::Edge::Bottom => (
                from.x + ((from.width as f32) * along) as i32,
                from.y
                    + if matches!(edge, inputshare_protocol::Edge::Bottom) {
                        from.height.saturating_sub(1) as i32
                    } else {
                        0
                    },
            ),
        };
        let (_x, _y, hit) = self.wrap_destination(from, edge, cx, cy, false)?;
        Some(hit)
    }

    /// If cursor sits on an outer edge of its monitor with no adjacent neighbour,
    /// return the wrapped (x, y) on the opposite extreme along the same axis.
    ///
    /// When `equalize` is true, the along-edge position is mapped proportionally
    /// onto the destination monitor (different resolutions). When false, absolute
    /// X (vertical wrap) / Y (horizontal wrap) is preserved and clamped.
    pub fn wrap_cursor_pos(&self, cx: i32, cy: i32, equalize: bool) -> Option<(i32, i32)> {
        self.wrap_cursor_target(cx, cy, equalize).map(|(x, y, _)| (x, y))
    }

    /// [`wrap_cursor_pos`](Self::wrap_cursor_pos), plus the display it lands on (it can belong
    /// to another computer: that wrap is a hand-off, not a cursor move).
    pub fn wrap_cursor_target(&self, cx: i32, cy: i32, equalize: bool) -> Option<(i32, i32, EdgeHit)> {
        let from = self.monitors.iter().find(|m| point_in(m, cx, cy))?;
        let edges = edges_touched(from, cx, cy);
        for edge in edges {
            let along = match edge {
                inputshare_protocol::Edge::Left | inputshare_protocol::Edge::Right => {
                    if from.height == 0 {
                        0.5
                    } else {
                        ((cy - from.y) as f32 / from.height as f32).clamp(0.0, 1.0)
                    }
                }
                inputshare_protocol::Edge::Top | inputshare_protocol::Edge::Bottom => {
                    if from.width == 0 {
                        0.5
                    } else {
                        ((cx - from.x) as f32 / from.width as f32).clamp(0.0, 1.0)
                    }
                }
            };
            // Adjacent monitor in that direction? OS already moves there — skip wrap.
            if self.neighbor_across(&from.id, edge, along).is_some() {
                continue;
            }
            if let Some(dest) = self.wrap_destination(from, edge, cx, cy, equalize) {
                return Some(dest);
            }
        }
        None
    }

    fn wrap_destination(
        &self,
        from: &MonitorTile,
        edge: inputshare_protocol::Edge,
        cx: i32,
        cy: i32,
        equalize: bool,
    ) -> Option<(i32, i32, EdgeHit)> {
        let others: Vec<&MonitorTile> = self
            .monitors
            .iter()
            .filter(|m| m.id != from.id)
            .collect();

        // Single display: wrap onto itself (opposite edge, same along).
        if others.is_empty() {
            let (x, y) = match edge {
                inputshare_protocol::Edge::Top => (cx, from.y + from.height as i32 - 2),
                inputshare_protocol::Edge::Bottom => (cx, from.y + 1),
                inputshare_protocol::Edge::Left => (from.x + from.width as i32 - 2, cy),
                inputshare_protocol::Edge::Right => (from.x + 1, cy),
            };
            let _ = equalize;
            return Some((
                x,
                y,
                EdgeHit {
                    monitor_id: from.id.clone(),
                    host: from.host.clone(),
                    entry_edge: opposite(edge),
                    entry: 0.5,
                },
            ));
        }

        let target = match edge {
            // ↑ keep X → monitor under that X with the lowest bottom edge
            inputshare_protocol::Edge::Top => {
                let mut cands: Vec<&MonitorTile> =
                    others.iter().copied().filter(|m| x_contains(m, cx)).collect();
                if cands.is_empty() {
                    cands = others
                        .iter()
                        .copied()
                        .filter(|m| x_ranges_overlap(m, from))
                        .collect();
                }
                cands
                    .into_iter()
                    .max_by_key(|m| m.y + m.height as i32)
            }
            // ↓ keep X → highest top
            inputshare_protocol::Edge::Bottom => {
                let mut cands: Vec<&MonitorTile> =
                    others.iter().copied().filter(|m| x_contains(m, cx)).collect();
                if cands.is_empty() {
                    cands = others
                        .iter()
                        .copied()
                        .filter(|m| x_ranges_overlap(m, from))
                        .collect();
                }
                cands.into_iter().min_by_key(|m| m.y)
            }
            // ← keep Y → rightmost monitor that spans this Y
            inputshare_protocol::Edge::Left => {
                let mut cands: Vec<&MonitorTile> =
                    others.iter().copied().filter(|m| y_contains(m, cy)).collect();
                if cands.is_empty() {
                    cands = others
                        .iter()
                        .copied()
                        .filter(|m| y_ranges_overlap(m, from))
                        .collect();
                }
                cands
                    .into_iter()
                    .max_by_key(|m| m.x + m.width as i32)
            }
            // → keep Y → leftmost
            inputshare_protocol::Edge::Right => {
                let mut cands: Vec<&MonitorTile> =
                    others.iter().copied().filter(|m| y_contains(m, cy)).collect();
                if cands.is_empty() {
                    cands = others
                        .iter()
                        .copied()
                        .filter(|m| y_ranges_overlap(m, from))
                        .collect();
                }
                cands.into_iter().min_by_key(|m| m.x)
            }
        }?;

        let along = match edge {
            inputshare_protocol::Edge::Left | inputshare_protocol::Edge::Right => {
                if from.height == 0 {
                    0.5
                } else {
                    ((cy - from.y) as f32 / from.height as f32).clamp(0.0, 1.0)
                }
            }
            inputshare_protocol::Edge::Top | inputshare_protocol::Edge::Bottom => {
                if from.width == 0 {
                    0.5
                } else {
                    ((cx - from.x) as f32 / from.width as f32).clamp(0.0, 1.0)
                }
            }
        };

        let (x, y, entry) = match edge {
            inputshare_protocol::Edge::Top => {
                let x = if equalize {
                    target.x + ((target.width.saturating_sub(1) as f32) * along) as i32
                } else {
                    cx.clamp(target.x, target.x + target.width as i32 - 1)
                };
                let y = target.y + target.height as i32 - 2;
                let entry = if target.width == 0 {
                    0.5
                } else {
                    ((x - target.x) as f32 / target.width as f32).clamp(0.0, 1.0)
                };
                (x, y, entry)
            }
            inputshare_protocol::Edge::Bottom => {
                let x = if equalize {
                    target.x + ((target.width.saturating_sub(1) as f32) * along) as i32
                } else {
                    cx.clamp(target.x, target.x + target.width as i32 - 1)
                };
                let y = target.y + 1;
                let entry = if target.width == 0 {
                    0.5
                } else {
                    ((x - target.x) as f32 / target.width as f32).clamp(0.0, 1.0)
                };
                (x, y, entry)
            }
            inputshare_protocol::Edge::Left => {
                let y = if equalize {
                    target.y + ((target.height.saturating_sub(1) as f32) * along) as i32
                } else {
                    cy.clamp(target.y, target.y + target.height as i32 - 1)
                };
                let x = target.x + target.width as i32 - 2;
                let entry = if target.height == 0 {
                    0.5
                } else {
                    ((y - target.y) as f32 / target.height as f32).clamp(0.0, 1.0)
                };
                (x, y, entry)
            }
            inputshare_protocol::Edge::Right => {
                let y = if equalize {
                    target.y + ((target.height.saturating_sub(1) as f32) * along) as i32
                } else {
                    cy.clamp(target.y, target.y + target.height as i32 - 1)
                };
                let x = target.x + 1;
                let entry = if target.height == 0 {
                    0.5
                } else {
                    ((y - target.y) as f32 / target.height as f32).clamp(0.0, 1.0)
                };
                (x, y, entry)
            }
        };

        Some((
            x,
            y,
            EdgeHit {
                monitor_id: target.id.clone(),
                host: target.host.clone(),
                entry_edge: opposite(edge),
                entry,
            },
        ))
    }
}

/// Which side of this computer's displays a newly seen computer is put on. Both computers
/// run the same rule on the same two names, so each one sees the other on the opposite side.
fn remote_goes_right(local_host: &str, remote_host: &str) -> bool {
    local_host.to_lowercase() < remote_host.to_lowercase()
}

impl MonitorLayout {
    /// Fill in the displays of a connected computer (`incoming`, in its own pixels).
    ///
    /// * Tiles of `remote_host` (or of its bare IP, from an older hand-made layout) are replaced
    ///   by one tile per incoming display, id `<host>:<label>`.
    /// * The whole computer keeps one offset, so its displays stay in the Windows arrangement.
    ///   A display the user dragged defines that offset; the others follow it instead of
    ///   keeping a position of their own.
    /// * A computer seen for the first time goes right or left of this one (see
    ///   [`remote_goes_right`]), with the two primary displays level at the top.
    /// * Displays that disappeared are dropped.
    ///
    /// Returns `true` when the layout changed.
    pub fn merge_remote_monitors(
        &mut self,
        local_host: &str,
        remote_host: &str,
        remote_ip: Option<&str>,
        incoming: &[inputshare_protocol::MonitorInfo],
    ) -> bool {
        if incoming.is_empty() || remote_host.eq_ignore_ascii_case(local_host) {
            return false;
        }
        let is_remote = |m: &MonitorTile| {
            m.host.eq_ignore_ascii_case(remote_host) || remote_ip.is_some_and(|ip| m.host == ip)
        };
        let old: Vec<MonitorTile> = self.monitors.iter().filter(|m| is_remote(m)).cloned().collect();
        let id_of = |label: &str| format!("{remote_host}:{label}");

        // Offset from the other computer's pixels to ours, learnt from a display we already placed.
        let known_offset = incoming.iter().find_map(|inc| {
            old.iter()
                .find(|t| t.id == id_of(&inc.label))
                .map(|t| (t.x - inc.x, t.y - inc.y))
        });
        let offset = known_offset.unwrap_or_else(|| self.first_placement_offset(local_host, remote_host, incoming));

        let placed: Vec<MonitorTile> = incoming
            .iter()
            .map(|inc| {
                let id = id_of(&inc.label);
                let (x, y) = (inc.x + offset.0, inc.y + offset.1);
                MonitorTile {
                    id,
                    host: remote_host.to_string(),
                    label: inc.label.clone(),
                    x,
                    y,
                    width: inc.width,
                    height: inc.height,
                    primary: inc.primary,
                    system_synced: false,
                }
            })
            .collect();

        let mut before = old;
        let mut after = placed.clone();
        before.sort_by(|a, b| a.id.cmp(&b.id));
        after.sort_by(|a, b| a.id.cmp(&b.id));
        if before == after {
            return false;
        }

        self.monitors.retain(|m| !is_remote(m));
        self.monitors.extend(placed);
        self.monitors
            .sort_by(|a, b| a.y.cmp(&b.y).then(a.x.cmp(&b.x)).then(a.label.cmp(&b.label)));
        true
    }

    /// Where a computer seen for the first time goes: beside the local displays, tops of the
    /// two primary displays level.
    fn first_placement_offset(
        &self,
        local_host: &str,
        remote_host: &str,
        incoming: &[inputshare_protocol::MonitorInfo],
    ) -> (i32, i32) {
        let local: Vec<&MonitorTile> = self
            .monitors
            .iter()
            .filter(|m| m.host.eq_ignore_ascii_case(local_host))
            .collect();
        if local.is_empty() {
            return (0, 0);
        }
        let local_primary = local.iter().find(|m| m.primary).unwrap_or(&local[0]);
        let remote_primary = incoming.iter().find(|m| m.primary).unwrap_or(&incoming[0]);
        let dy = local_primary.y - remote_primary.y;
        let dx = if remote_goes_right(local_host, remote_host) {
            let local_right = local.iter().map(|m| m.x + m.width as i32).max().unwrap_or(0);
            let remote_left = incoming.iter().map(|m| m.x).min().unwrap_or(0);
            local_right - remote_left
        } else {
            let local_left = local.iter().map(|m| m.x).min().unwrap_or(0);
            let remote_right = incoming.iter().map(|m| m.x + m.width as i32).max().unwrap_or(0);
            local_left - remote_right
        };
        (dx, dy)
    }
}

/// Where the cursor is on another computer while this one steers it, tracked here from the
/// mouse deltas that are sent. It exists so the user can always come back: moving past the edge
/// they entered through hands control back, and nothing depends on the other computer answering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpace {
    /// Bounding box of the other computer's displays (layout space): `min` inclusive, `max` exclusive.
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
    /// Virtual cursor.
    pub x: i32,
    pub y: i32,
    /// Edge of the entered display the cursor came in through; leaving goes back out through it.
    pub exit: inputshare_protocol::Edge,
    /// The entered display `(x0, y0, x1, y1)`, x1/y1 exclusive: leaving is allowed along its exit edge.
    pub tile: (i32, i32, i32, i32),
    /// Other edges of the other computer's displays that lead to this computer (a neighbour, or
    /// the monitor loop), see [`RemoteSpace::with_returns`].
    pub returns: Vec<ReturnEdge>,
    /// Where the cursor lands on this computer after leaving through one of `returns`
    /// (`None` after leaving through `exit`: back where it was).
    pub landing: Option<(i32, i32)>,
}

/// An edge of a display on the steered computer that leads back to a display of this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReturnEdge {
    /// Moving this way out of `from` ...
    pub side: inputshare_protocol::Edge,
    /// ... the steered computer's display `(x0, y0, x1, y1)`, x1/y1 exclusive ...
    pub from: (i32, i32, i32, i32),
    /// ... lands on this computer's display (same rect form, layout = screen coordinates).
    pub to: (i32, i32, i32, i32),
}

impl ReturnEdge {
    /// Cursor point on `to` for leaving `from` at `(x, y)`, proportional along the edge.
    fn land(&self, x: i32, y: i32) -> (i32, i32) {
        use inputshare_protocol::Edge::*;
        let (fx0, fy0, fx1, fy1) = self.from;
        let (tx0, ty0, tx1, ty1) = self.to;
        let frac = |v: i32, a: i32, b: i32| ((v - a) as f32 / (b - a).max(1) as f32).clamp(0.0, 1.0);
        let spread = |f: f32, a: i32, b: i32| a + (f * (b - a - 1).max(0) as f32) as i32;
        match self.side {
            Left | Right => {
                let y = spread(frac(y, fy0, fy1), ty0, ty1);
                let x = if self.side == Right { tx0 + 1 } else { tx1 - 2 };
                (x, y)
            }
            Top | Bottom => {
                let x = spread(frac(x, fx0, fx1), tx0, tx1);
                let y = if self.side == Bottom { ty0 + 1 } else { ty1 - 2 };
                (x, y)
            }
        }
    }
}

impl RemoteSpace {
    /// Start where the cursor enters: on `entry_edge` of tile `tile_id` of `host`, `entry` (0..1)
    /// of the way along it. `None` when the layout has no such display.
    pub fn enter(
        layout: &MonitorLayout,
        host: &str,
        tile_id: Option<&str>,
        entry_edge: inputshare_protocol::Edge,
        entry: f32,
    ) -> Option<Self> {
        use inputshare_protocol::Edge::*;
        let tiles: Vec<&MonitorTile> = layout
            .monitors
            .iter()
            .filter(|m| m.host.eq_ignore_ascii_case(host))
            .collect();
        let hit = tile_id
            .and_then(|id| tiles.iter().copied().find(|m| m.id == id))
            .or_else(|| tiles.first().copied())?;
        let (x0, y0) = (hit.x, hit.y);
        let (x1, y1) = (hit.x + hit.width as i32, hit.y + hit.height as i32);
        let entry = entry.clamp(0.0, 1.0);
        let along_x = x0 + ((x1 - x0 - 1) as f32 * entry) as i32;
        let along_y = y0 + ((y1 - y0 - 1) as f32 * entry) as i32;
        let (x, y) = match entry_edge {
            Left => (x0, along_y),
            Right => (x1 - 1, along_y),
            Top => (along_x, y0),
            Bottom => (along_x, y1 - 1),
        };
        Some(Self {
            min_x: tiles.iter().map(|m| m.x).min()?,
            min_y: tiles.iter().map(|m| m.y).min()?,
            max_x: tiles.iter().map(|m| m.x + m.width as i32).max()?,
            max_y: tiles.iter().map(|m| m.y + m.height as i32).max()?,
            x,
            y,
            exit: entry_edge,
            tile: (x0, y0, x1, y1),
            returns: Vec::new(),
            landing: None,
        })
    }

    /// Add the other ways back to `local_host`: every edge of `host`'s displays whose neighbour
    /// (or, with `wrap`, the monitor the loop jumps to) is one of this computer's displays.
    /// The edge the cursor came in by stays the plain way back.
    pub fn with_returns(mut self, layout: &MonitorLayout, host: &str, local_host: &str, wrap: bool) -> Self {
        use inputshare_protocol::Edge::*;
        let rect = |m: &MonitorTile| (m.x, m.y, m.x + m.width as i32, m.y + m.height as i32);
        for m in layout.monitors.iter().filter(|m| m.host.eq_ignore_ascii_case(host)) {
            for side in [Left, Right, Top, Bottom] {
                if rect(m) == self.tile && side == self.exit {
                    continue;
                }
                let Some(hit) = layout.neighbor_or_wrap(&m.id, side, 0.5, wrap) else {
                    continue;
                };
                if !hit.host.eq_ignore_ascii_case(local_host) {
                    continue;
                }
                if let Some(to) = layout.monitors.iter().find(|t| t.id == hit.monitor_id) {
                    self.returns.push(ReturnEdge { side, from: rect(m), to: rect(to) });
                }
            }
        }
        self
    }

    /// Move the virtual cursor by one sent delta. Returns `true` when it went out through the exit
    /// edge (only when `can_leave`, so the jitter of the push that carried the cursor over the edge
    /// does not bounce it straight back). Anywhere else the cursor stops at the border of the
    /// other computer's displays.
    pub fn advance(&mut self, dx: i32, dy: i32, can_leave: bool) -> bool {
        self.advance_with(dx, dy, |_| can_leave)
    }

    /// [`advance`](Self::advance), with leaving decided per direction of travel (`Left` =
    /// moving left out of a display), e.g. by the speed of the push that way.
    pub fn advance_with(&mut self, dx: i32, dy: i32, can_leave: impl Fn(inputshare_protocol::Edge) -> bool) -> bool {
        use inputshare_protocol::Edge::*;
        self.landing = None;
        let nx = self.x.saturating_add(dx);
        let ny = self.y.saturating_add(dy);
        let (x0, y0, x1, y1) = self.tile;
        let along_x = (x0..x1).contains(&nx);
        let along_y = (y0..y1).contains(&ny);
        let out = match self.exit {
            Left => nx < x0 && along_y,
            Right => nx >= x1 && along_y,
            Top => ny < y0 && along_x,
            Bottom => ny >= y1 && along_x,
        };
        if out && can_leave(self.exit) {
            return true;
        }
        {
            let (x, y) = (self.x, self.y);
            let back = self.returns.iter().find(|r| {
                let (x0, y0, x1, y1) = r.from;
                let inside = (x0..x1).contains(&x) && (y0..y1).contains(&y);
                inside
                    && can_leave(r.side)
                    && match r.side {
                        Left => nx < x0 && (y0..y1).contains(&ny),
                        Right => nx >= x1 && (y0..y1).contains(&ny),
                        Top => ny < y0 && (x0..x1).contains(&nx),
                        Bottom => ny >= y1 && (x0..x1).contains(&nx),
                    }
            });
            if let Some(r) = back {
                self.landing = Some(r.land(nx, ny));
                return true;
            }
        }
        self.x = nx.clamp(self.min_x, (self.max_x - 1).max(self.min_x));
        self.y = ny.clamp(self.min_y, (self.max_y - 1).max(self.min_y));
        false
    }
}

/// Hostname used for local tiles.
pub fn local_hostname() -> String {
    hostname_or("THIS-PC")
}

fn hostname_or(fallback: &str) -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| fallback.to_string())
}

#[derive(Debug, Clone)]
pub struct EdgeHit {
    pub monitor_id: String,
    pub host: String,
    pub entry_edge: inputshare_protocol::Edge,
    pub entry: f32,
}

/// How far a facing pair still counts as "next to each other": half the shorter side.
fn glue_reach(a: u32, b: u32) -> i32 {
    ((a.min(b) as i32) / 2).clamp(160, 800)
}

fn edge_hit(
    edge: inputshare_protocol::Edge,
    m: &MonitorTile,
    probe_x: i32,
    probe_y: i32,
) -> EdgeHit {
    use inputshare_protocol::Edge::*;
    let entry = match edge {
        Left | Right => {
            if m.height == 0 {
                0.5
            } else {
                ((probe_y - m.y) as f32 / m.height as f32).clamp(0.0, 1.0)
            }
        }
        Top | Bottom => {
            if m.width == 0 {
                0.5
            } else {
                ((probe_x - m.x) as f32 / m.width as f32).clamp(0.0, 1.0)
            }
        }
    };
    EdgeHit {
        monitor_id: m.id.clone(),
        host: m.host.clone(),
        entry_edge: opposite(edge),
        entry,
    }
}

fn opposite(e: inputshare_protocol::Edge) -> inputshare_protocol::Edge {
    use inputshare_protocol::Edge::*;
    match e {
        Left => Right,
        Right => Left,
        Top => Bottom,
        Bottom => Top,
    }
}

fn point_in(m: &MonitorTile, x: i32, y: i32) -> bool {
    x >= m.x && x < m.x + m.width as i32 && y >= m.y && y < m.y + m.height as i32
}

fn x_contains(m: &MonitorTile, x: i32) -> bool {
    x >= m.x && x < m.x + m.width as i32
}

fn y_contains(m: &MonitorTile, y: i32) -> bool {
    y >= m.y && y < m.y + m.height as i32
}

fn x_ranges_overlap(a: &MonitorTile, b: &MonitorTile) -> bool {
    a.x < b.x + b.width as i32 && b.x < a.x + a.width as i32
}

fn y_ranges_overlap(a: &MonitorTile, b: &MonitorTile) -> bool {
    a.y < b.y + b.height as i32 && b.y < a.y + a.height as i32
}

fn edges_touched(m: &MonitorTile, cx: i32, cy: i32) -> Vec<inputshare_protocol::Edge> {
    use inputshare_protocol::Edge::*;
    let mut edges = Vec::new();
    // 2px tolerance — Windows clamps cursor to the last pixel on the edge.
    if cy <= m.y + 1 {
        edges.push(Top);
    }
    if cy >= m.y + m.height as i32 - 2 {
        edges.push(Bottom);
    }
    if cx <= m.x + 1 {
        edges.push(Left);
    }
    if cx >= m.x + m.width as i32 - 2 {
        edges.push(Right);
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    /// User's real desk: A above B, D–B–C row (D slightly lower).
    fn skull_desk() -> MonitorLayout {
        MonitorLayout {
            monitors: vec![
                MonitorTile {
                    id: "a".into(),
                    host: "SKULLPC".into(),
                    label: "A".into(),
                    x: 320,
                    y: -1080,
                    width: 1920,
                    height: 1080,
                    primary: false,
                    system_synced: true,
                },
                MonitorTile {
                    id: "b".into(),
                    host: "SKULLPC".into(),
                    label: "B".into(),
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                    primary: true,
                    system_synced: true,
                },
                MonitorTile {
                    id: "c".into(),
                    host: "SKULLPC".into(),
                    label: "C".into(),
                    x: 2560,
                    y: 0,
                    width: 2560,
                    height: 1440,
                    primary: false,
                    system_synced: true,
                },
                MonitorTile {
                    id: "d".into(),
                    host: "SKULLPC".into(),
                    label: "D".into(),
                    x: -2560,
                    y: 400,
                    width: 2560,
                    height: 1440,
                    primary: false,
                    system_synced: true,
                },
            ],
        }
    }

    #[test]
    fn up_from_a_wraps_to_bottom_of_b_same_x() {
        let layout = skull_desk();
        // Center-top of A
        let cx = 320 + 960;
        let cy = -1080;
        let (nx, ny) = layout.wrap_cursor_pos(cx, cy, false).expect("wrap");
        assert!(
            point_in(layout.monitors.iter().find(|m| m.id == "b").unwrap(), nx, ny)
                || ny >= 1440 - 5,
            "expected near bottom of B, got ({nx},{ny})"
        );
        assert_eq!(nx, cx, "X must stay the same");
        assert!(ny >= 1440 - 5, "should be near bottom of B (y≈1438), got {ny}");
    }

    #[test]
    fn left_from_d_wraps_to_right_of_c() {
        let layout = skull_desk();
        let cx = -2560;
        let cy = 400 + 720;
        let (nx, ny) = layout.wrap_cursor_pos(cx, cy, false).expect("wrap");
        assert_eq!(ny, cy, "Y must stay the same");
        // Right edge of C: x = 2560+2560-2 = 5118
        assert!(nx >= 2560 + 2560 - 5, "expected right of C, got {nx}");
    }

    #[test]
    fn right_from_c_wraps_to_left_of_d() {
        let layout = skull_desk();
        let cx = 2560 + 2560 - 1;
        let cy = 720;
        let (nx, ny) = layout.wrap_cursor_pos(cx, cy, false).expect("wrap");
        assert_eq!(ny, cy);
        assert!(nx <= -2560 + 5, "expected left of D, got {nx}");
    }

    #[test]
    fn right_edge_finds_neighbor_no_wrap() {
        let layout = skull_desk();
        // Middle of B right edge — neighbor C exists, wrap_cursor should NOT fire
        // (neighbor check skips wrap). Cursor on B near right but OS would move to C;
        // our function returns None when neighbor exists.
        let cx = 2560 - 1;
        let cy = 720;
        assert!(layout.wrap_cursor_pos(cx, cy, false).is_none());
    }

    #[test]
    fn equalize_maps_along_edge_proportionally() {
        // Tall 1080p above wide 2560 — mid-top of A should land mid-bottom of B.
        let layout = MonitorLayout {
            monitors: vec![
                MonitorTile {
                    id: "a".into(),
                    host: "PC".into(),
                    label: "A".into(),
                    x: 320,
                    y: -1080,
                    width: 1920,
                    height: 1080,
                    primary: false,
                    system_synced: true,
                },
                MonitorTile {
                    id: "b".into(),
                    host: "PC".into(),
                    label: "B".into(),
                    x: 0,
                    y: 0,
                    width: 2560,
                    height: 1440,
                    primary: true,
                    system_synced: true,
                },
            ],
        };
        // 75% along A's top edge
        let cx = 320 + ((1920 as f32) * 0.75) as i32;
        let cy = -1080;
        let (nx, ny) = layout.wrap_cursor_pos(cx, cy, true).expect("wrap");
        let expected_x = 0 + ((2560u32.saturating_sub(1) as f32) * 0.75) as i32;
        assert!(
            (nx - expected_x).abs() <= 2,
            "equalize X: got {nx}, expected ~{expected_x}"
        );
        assert!(ny >= 1440 - 5, "near bottom of B, got {ny}");

        // Without equalize, absolute X is kept (clamped into B).
        let (ax, _) = layout.wrap_cursor_pos(cx, cy, false).expect("wrap");
        assert_eq!(ax, cx.clamp(0, 2559));
    }
}

#[cfg(test)]
mod remote_merge_tests {
    use super::*;
    use inputshare_protocol::MonitorInfo;

    fn mon(label: &str, x: i32, y: i32, w: u32, h: u32, primary: bool) -> MonitorInfo {
        MonitorInfo { label: label.into(), x, y, width: w, height: h, primary }
    }

    fn tile(id: &str, host: &str, x: i32, y: i32, w: u32, h: u32, primary: bool, synced: bool) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: host.into(),
            label: id.rsplit(':').next().unwrap_or(id).into(),
            x,
            y,
            width: w,
            height: h,
            primary,
            system_synced: synced,
        }
    }

    /// This computer: A above-left (292,-1080), primary B at the origin.
    fn local() -> MonitorLayout {
        MonitorLayout {
            monitors: vec![
                tile("sys-a", "SKULLPC", 292, -1080, 1920, 1080, false, true),
                tile("sys-b", "SKULLPC", 0, 0, 2560, 1440, true, true),
            ],
        }
    }

    fn find<'a>(l: &'a MonitorLayout, id: &str) -> &'a MonitorTile {
        l.monitors.iter().find(|m| m.id == id).unwrap_or_else(|| panic!("no tile {id}"))
    }

    #[test]
    fn first_seen_computer_goes_right_of_ours_with_primary_tops_level() {
        let mut l = local();
        let changed = l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[mon("A", 0, 0, 1920, 1080, true)]);
        assert!(changed);
        let t = find(&l, "TESTPC:A");
        assert_eq!((t.x, t.y), (2560, 0));
        assert_eq!(t.host, "TESTPC");
        assert!(!t.system_synced, "remote tiles stay draggable");
        assert_eq!(l.monitors.len(), 3);
    }

    #[test]
    fn a_name_that_sorts_before_ours_goes_left() {
        let mut l = local();
        l.merge_remote_monitors("SKULLPC", "LAPTOP", None, &[mon("A", 0, 0, 1920, 1080, true)]);
        assert_eq!(find(&l, "LAPTOP:A").x, -1920);
    }

    #[test]
    fn both_computers_see_each_other_on_opposite_sides() {
        let one = |l: &str, r: &str| {
            let mut layout = MonitorLayout { monitors: vec![tile("s", l, 0, 0, 1920, 1080, true, true)] };
            layout.merge_remote_monitors(l, r, None, &[mon("A", 0, 0, 1920, 1080, true)]);
            find(&layout, &format!("{r}:A")).x
        };
        assert_eq!(one("PC-ONE", "PC-TWO"), 1920, "PC-TWO sits to the right of PC-ONE");
        assert_eq!(one("PC-TWO", "PC-ONE"), -1920, "PC-ONE sits to the left of PC-TWO");
        assert_eq!(one("pc-one", "PC-TWO"), 1920, "case does not matter");
    }

    #[test]
    fn a_dragged_tile_keeps_its_place_while_size_follows_the_other_computer() {
        let mut l = local();
        l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[mon("A", 0, 0, 1920, 1080, true)]);
        let dragged = l.monitors.iter_mut().find(|m| m.id == "TESTPC:A").unwrap();
        dragged.x = 320;
        dragged.y = -1440;
        assert!(l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[mon("A", 0, 0, 2560, 1440, true)]));
        let t = find(&l, "TESTPC:A");
        assert_eq!((t.x, t.y, t.width, t.height), (320, -1440, 2560, 1440));
        assert!(
            !l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[mon("A", 0, 0, 2560, 1440, true)]),
            "the same list again changes nothing"
        );
    }

    #[test]
    fn a_new_display_lands_beside_the_placed_ones_and_an_unplugged_one_goes_away() {
        let mut l = local();
        l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[mon("A", 0, 0, 1920, 1080, true)]);
        l.monitors.iter_mut().find(|m| m.id == "TESTPC:A").unwrap().x = 4000;
        let two = [mon("A", 0, 0, 1920, 1080, true), mon("B", 1920, 0, 1920, 1080, false)];
        assert!(l.merge_remote_monitors("SKULLPC", "TESTPC", None, &two));
        assert_eq!(find(&l, "TESTPC:B").x, 4000 + 1920, "same offset as the placed display");
        assert!(l.merge_remote_monitors("SKULLPC", "TESTPC", None, &two[..1]));
        assert!(!l.monitors.iter().any(|m| m.id == "TESTPC:B"));
        assert_eq!(l.monitors.iter().filter(|m| m.host == "SKULLPC").count(), 2, "ours are untouched");
    }

    #[test]
    fn tiles_made_by_hand_under_the_ip_are_adopted() {
        let mut l = local();
        l.monitors.push(tile("remote-1", "192.168.100.52", 2560, 0, 1920, 1080, false, false));
        l.merge_remote_monitors("SKULLPC", "TESTPC", Some("192.168.100.52"), &[mon("A", 0, 0, 1920, 1080, true)]);
        assert!(!l.monitors.iter().any(|m| m.host == "192.168.100.52"));
        assert_eq!(l.monitors.iter().filter(|m| m.host == "TESTPC").count(), 1);
    }

    #[test]
    fn nothing_happens_for_our_own_name_or_an_empty_list() {
        let mut l = local();
        assert!(!l.merge_remote_monitors("SKULLPC", "skullpc", None, &[mon("A", 0, 0, 1920, 1080, true)]));
        assert!(!l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[]));
        assert_eq!(l.monitors.len(), 2);
    }

    #[test]
    fn a_computer_stays_in_its_windows_arrangement_when_one_display_was_dragged() {
        let mut l = local();
        let two = [mon("A", 0, 0, 1920, 1080, true), mon("B", 1920, 100, 1280, 1024, false)];
        l.merge_remote_monitors("SKULLPC", "TESTPC", None, &two);
        // Drag only B. The next report from that computer puts it back beside A.
        l.monitors.iter_mut().find(|m| m.id == "TESTPC:B").unwrap().x += 400;
        l.merge_remote_monitors("SKULLPC", "TESTPC", None, &two);
        let a = find(&l, "TESTPC:A");
        let b = find(&l, "TESTPC:B");
        assert_eq!(b.x - a.x, 1920);
        assert_eq!(b.y - a.y, 100);
    }

    #[test]
    fn a_placed_remote_display_is_reachable_across_the_edge() {
        let mut l = local();
        l.merge_remote_monitors("SKULLPC", "TESTPC", None, &[mon("A", 0, 0, 1920, 1080, true)]);
        let hit = l
            .neighbor_across("sys-b", inputshare_protocol::Edge::Right, 0.5)
            .expect("remote display right of B");
        assert_eq!(hit.host, "TESTPC");
        assert_eq!(hit.monitor_id, "TESTPC:A");
    }
}

#[cfg(test)]
mod remote_space_tests {
    use super::*;
    use inputshare_protocol::Edge;

    fn tile(id: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: "PC2".into(),
            label: id.into(),
            x,
            y,
            width: w,
            height: h,
            primary: false,
            system_synced: false,
        }
    }

    /// PC2: one 1920x1080 display at (2560, 0), entered through its left edge.
    fn single() -> RemoteSpace {
        let layout = MonitorLayout { monitors: vec![tile("PC2:A", 2560, 0, 1920, 1080)] };
        RemoteSpace::enter(&layout, "pc2", Some("PC2:A"), Edge::Left, 0.5).expect("space")
    }

    #[test]
    fn it_starts_on_the_entry_edge_at_the_entry_fraction() {
        let s = single();
        assert_eq!((s.x, s.y), (2560, 539));
        let layout = MonitorLayout { monitors: vec![tile("PC2:A", 2560, 0, 1920, 1080)] };
        let r = RemoteSpace::enter(&layout, "PC2", None, Edge::Right, 0.0).unwrap();
        assert_eq!((r.x, r.y), (2560 + 1919, 0));
        let t = RemoteSpace::enter(&layout, "PC2", None, Edge::Top, 1.0).unwrap();
        assert_eq!((t.x, t.y), (2560 + 1919, 0));
        let b = RemoteSpace::enter(&layout, "PC2", None, Edge::Bottom, 0.5).unwrap();
        assert_eq!(b.y, 1079);
    }

    #[test]
    fn there_is_no_space_for_a_host_without_displays() {
        let layout = MonitorLayout { monitors: vec![tile("PC2:A", 0, 0, 100, 100)] };
        assert!(RemoteSpace::enter(&layout, "OTHER", None, Edge::Left, 0.5).is_none());
    }

    #[test]
    fn pushing_back_through_the_entry_edge_leaves() {
        let mut s = single();
        assert!(!s.advance(300, 0, true), "moving into the display does not leave");
        assert_eq!(s.x, 2860);
        assert!(!s.advance(-299, 0, true), "one pixel short of the edge");
        assert!(s.advance(-5, 0, true), "past the edge: back to the first computer");
    }

    #[test]
    fn the_first_moments_after_entering_cannot_bounce_the_cursor_back() {
        let mut s = single();
        assert!(!s.advance(-3, 0, false), "jitter right after entering stays on the other computer");
        assert_eq!(s.x, 2560, "and stops at the edge instead of leaving the display");
        assert!(s.advance(-1, 0, true));
    }

    #[test]
    fn the_other_borders_are_walls() {
        let mut s = single();
        assert!(!s.advance(10_000, 0, true));
        assert_eq!(s.x, 2560 + 1919);
        assert!(!s.advance(0, -10_000, true));
        assert_eq!(s.y, 0);
        assert!(!s.advance(0, 10_000, true));
        assert_eq!(s.y, 1079);
    }

    #[test]
    fn leaving_needs_the_cursor_to_be_beside_the_entered_display() {
        // Entered through the lower display's left edge; the upper one sticks out to the left.
        let layout = MonitorLayout {
            monitors: vec![tile("PC2:A", 2000, 0, 1000, 500), tile("PC2:B", 2560, 500, 1920, 500)],
        };
        let mut s = RemoteSpace::enter(&layout, "PC2", Some("PC2:B"), Edge::Left, 0.5).unwrap();
        assert!(!s.advance(0, -300, true));
        // The cursor is now level with the upper display (above display B): moving left there is
        // not "back through the entry edge", it is a move on the other computer.
        assert!(!s.advance(-100, 0, true));
        assert_eq!(s.x, 2460);
    }

    #[test]
    fn every_edge_leaves_in_its_own_direction() {
        let layout = MonitorLayout { monitors: vec![tile("PC2:A", 0, 0, 1000, 1000)] };
        for (edge, dx, dy) in [(Edge::Left, -1, 0), (Edge::Right, 1, 0), (Edge::Top, 0, -1), (Edge::Bottom, 0, 1)] {
            let mut s = RemoteSpace::enter(&layout, "PC2", None, edge, 0.5).unwrap();
            assert!(s.advance(dx, dy, true), "{edge:?}");
        }
    }
}

#[cfg(test)]
mod snap_tests {
    use super::*;
    use inputshare_protocol::Edge;

    fn tile(id: &str, host: &str, x: i32, y: i32, w: u32, h: u32, synced: bool) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: host.into(),
            label: id.into(),
            x,
            y,
            width: w,
            height: h,
            primary: false,
            system_synced: synced,
        }
    }

    #[test]
    fn dragging_moves_the_whole_computer_and_leaves_windows_arrangement() {
        let mut l = MonitorLayout {
            monitors: vec![
                tile("L", "LOCAL", 0, 0, 1920, 1080, true),
                tile("A", "PC2", 2000, 0, 1920, 1080, false),
                tile("B", "PC2", 3920, 100, 1280, 1024, false),
            ],
        };
        assert!(l.translate_host("PC2", -40, 10));
        let a = l.monitors.iter().find(|m| m.id == "A").unwrap();
        let b = l.monitors.iter().find(|m| m.id == "B").unwrap();
        assert_eq!((a.x, a.y), (1960, 10));
        assert_eq!(b.x - a.x, 1920);
        assert_eq!(b.y - a.y, 100);
        assert!(!l.translate_host("LOCAL", 100, 0), "this computer stays where Windows put it");
        assert_eq!(l.monitors.iter().find(|m| m.id == "L").unwrap().x, 0);
    }

    #[test]
    fn a_near_edge_glues_flush_and_a_far_one_does_not() {
        let mut l = MonitorLayout {
            monitors: vec![
                tile("L", "LOCAL", 0, 0, 1920, 1080, true),
                tile("A", "PC2", 1920 + 40, 0, 1920, 1080, false),
            ],
        };
        assert!(l.snap_host("PC2"));
        assert_eq!(l.monitors.iter().find(|m| m.id == "A").unwrap().x, 1920);
        // Half of the shorter side (1080/2 = 540) still counts as next to the edge.
        l.monitors.iter_mut().find(|m| m.id == "A").unwrap().x = 1920 + 400;
        assert!(l.snap_host("PC2"));
        assert_eq!(l.monitors.iter().find(|m| m.id == "A").unwrap().x, 1920);
        l.monitors.iter_mut().find(|m| m.id == "A").unwrap().x = 1920 + 2000;
        assert!(!l.snap_host("PC2"));
        assert_eq!(l.monitors.iter().find(|m| m.id == "A").unwrap().x, 1920 + 2000);
    }

    #[test]
    fn gluing_closes_the_gap_but_keeps_the_screen_where_it_was_dropped_along_the_edge() {
        let mut l = MonitorLayout {
            monitors: vec![
                tile("L", "LOCAL", 0, 0, 2560, 1440, true),
                // 400px to the right, and 210px lower than the tall screen — the layout from the desk.
                tile("B", "PC2", 2560 + 400, 210, 1920, 1080, false),
            ],
        };
        assert!(l.snap_host("PC2"));
        let b = l.monitors.iter().find(|m| m.id == "B").unwrap();
        assert_eq!(b.x, 2560, "the edges meet");
        assert_eq!(b.y, 210, "not pulled to the top or bottom corner");
    }

    #[test]
    fn a_crack_still_crosses_and_a_wide_gap_does_not() {
        let near = MonitorLayout {
            monitors: vec![
                tile("L", "LOCAL", 0, 0, 1920, 1080, true),
                tile("A", "PC2", 1920 + 20, 0, 1920, 1080, false),
            ],
        };
        let hit = near.neighbor_across("L", Edge::Right, 0.5).expect("20px crack");
        assert_eq!(hit.host, "PC2");
        assert_eq!(hit.entry_edge, Edge::Left);
        let far = MonitorLayout {
            monitors: vec![
                tile("L", "LOCAL", 0, 0, 1920, 1080, true),
                tile("A", "PC2", 1920 + 400, 0, 1920, 1080, false),
            ],
        };
        assert!(far.neighbor_across("L", Edge::Right, 0.5).is_none());
    }

    #[test]
    fn a_windows_refresh_closes_a_crack_but_not_a_real_gap() {
        let mut l = MonitorLayout {
            monitors: vec![
                tile("old", "LOCAL", 0, 0, 1920, 1080, true),
                tile("A", "PC2", 1940, 0, 800, 600, false),
                tile("B", "FAR", 5000, 0, 800, 600, false),
            ],
        };
        l.merge_system_displays(MonitorLayout {
            monitors: vec![tile("L", "LOCAL", 0, 0, 1920, 1080, true)],
        });
        assert_eq!(l.monitors.iter().find(|m| m.id == "A").unwrap().x, 1920, "crack closed onto the new edge");
        assert_eq!(l.monitors.iter().find(|m| m.id == "B").unwrap().x, 5000, "a computer a full screen away stays there");
        assert!(l.monitors.iter().any(|m| m.id == "L"));
        assert!(!l.monitors.iter().any(|m| m.id == "old"));
    }
}

/// The desk from the screenshot: SKULLPC (A above B) on the left, then DESKTOP-LU3CPA3 with B
/// next to SKULLPC and A on the far right, "loop the monitors" on.
#[cfg(test)]
mod loop_across_computers_tests {
    use super::*;
    use inputshare_protocol::Edge;

    fn tile(id: &str, host: &str, x: i32, y: i32, w: u32, h: u32) -> MonitorTile {
        MonitorTile {
            id: id.into(),
            host: host.into(),
            label: id.into(),
            x,
            y,
            width: w,
            height: h,
            primary: false,
            system_synced: host == "SKULLPC",
        }
    }

    fn desk() -> MonitorLayout {
        MonitorLayout {
            monitors: vec![
                tile("SKULLPC:A", "SKULLPC", 140, -1080, 1920, 1080),
                tile("SKULLPC:B", "SKULLPC", 0, 0, 2560, 1440),
                tile("LU3:B", "LU3", 2560, 120, 1920, 1080),
                tile("LU3:A", "LU3", 4480, 0, 2560, 1440),
            ],
        }
    }

    #[test]
    fn the_left_edge_of_skullpc_loops_to_the_far_right_display_of_the_other_computer() {
        let (_, _, hit) = desk().wrap_cursor_target(0, 700, false).expect("loop target");
        assert_eq!(hit.monitor_id, "LU3:A");
        assert_eq!(hit.entry_edge, Edge::Right, "comes in through its right edge");
    }

    #[test]
    fn past_the_far_right_display_the_loop_brings_the_cursor_home_to_the_left_edge() {
        let layout = desk();
        let mut s = RemoteSpace::enter(&layout, "LU3", Some("LU3:B"), Edge::Left, 0.5)
            .unwrap()
            .with_returns(&layout, "LU3", "SKULLPC", true);
        assert_eq!(s.returns.len(), 1, "{:?}", s.returns);
        // across LU3:B into LU3:A, up to its right edge (stops there), then one more push
        assert!(!s.advance(1920 + 2550, 0, true));
        assert!(s.advance(20, 0, true), "out through the right edge of LU3:A");
        let (x, y) = s.landing.expect("lands on SKULLPC:B");
        assert_eq!(x, 1, "just inside the left edge of SKULLPC:B");
        assert!((0..1440).contains(&y));

        // without the loop the far right edge is a wall
        let mut s = RemoteSpace::enter(&layout, "LU3", Some("LU3:B"), Edge::Left, 0.5)
            .unwrap()
            .with_returns(&layout, "LU3", "SKULLPC", false);
        assert!(s.returns.is_empty());
        assert!(!s.advance(1920 + 2550, 0, true));
        assert!(!s.advance(20, 0, true));
    }

    #[test]
    fn leaving_is_decided_for_the_direction_of_the_edge_being_crossed() {
        let layout = desk();
        let enter = || {
            RemoteSpace::enter(&layout, "LU3", Some("LU3:B"), Edge::Left, 0.5)
                .unwrap()
                .with_returns(&layout, "LU3", "SKULLPC", true)
        };
        // the loop edge on the far right is crossed moving right
        let mut s = enter();
        assert!(!s.advance(1920 + 2550, 0, true));
        assert!(!s.advance_with(20, 0, |e| e == Edge::Left), "only a push to the left would count");
        assert!(s.advance_with(20, 0, |e| e == Edge::Right));
        // the entry edge on the left is crossed moving left
        let mut s = enter();
        assert!(!s.advance_with(-20, 0, |e| e == Edge::Right));
        assert!(s.advance_with(-20, 0, |e| e == Edge::Left));
    }

    #[test]
    fn coming_in_by_the_loop_goes_back_the_same_way_and_also_through_the_direct_neighbour() {
        let layout = desk();
        let mut s = RemoteSpace::enter(&layout, "LU3", Some("LU3:A"), Edge::Right, 0.5)
            .unwrap()
            .with_returns(&layout, "LU3", "SKULLPC", true);
        assert!(s.advance(10, 0, true), "back out through the edge it came in by");
        assert_eq!(s.landing, None, "that one goes back where the cursor was");
        // LU3:B's left edge touches SKULLPC:B directly
        assert!(s.returns.iter().any(|r| r.side == Edge::Left && r.to == (0, 0, 2560, 1440)));
    }
}
