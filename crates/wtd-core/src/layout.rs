//! Runtime binary pane layout tree (§18).
//!
//! Each tab owns one [`LayoutTree`]. Leaf nodes are panes identified by
//! [`PaneId`]; internal nodes are split containers with an [`Orientation`] and
//! a ratio.

use std::collections::HashMap;

use crate::ids::PaneId;
use crate::workspace::{Orientation, PaneLeaf, PaneNode, SplitNode};

/// Minimum pane width in character cells (§18.4).
pub const MIN_PANE_COLS: u16 = 2;
/// Minimum pane height in character cells (§18.4).
pub const MIN_PANE_ROWS: u16 = 1;

// ── Public helper types ───────────────────────────────────────────────────────

/// Axis-aligned rectangle in character-cell coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn center_x(self) -> f64 {
        self.x as f64 + self.width as f64 / 2.0
    }

    fn center_y(self) -> f64 {
        self.y as f64 + self.height as f64 / 2.0
    }
}

/// Direction for spatial focus movement (§18.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Direction for spatial pane resize movement (§18.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeDirection {
    GrowRight,
    GrowDown,
    ShrinkRight,
    ShrinkDown,
}

/// Outcome of [`LayoutTree::close_pane`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseResult {
    /// Pane removed; focus moved to the returned pane.
    Closed { new_focus: PaneId },
    /// Last pane removed; the tree is now empty.
    LastClosed,
}

/// Errors from layout operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("pane {0} not found")]
    PaneNotFound(PaneId),
}

// ── Internal node representation ──────────────────────────────────────────────

type Idx = usize;

#[derive(Debug, Clone)]
enum LayoutPlan {
    Pane(PaneId),
    Split {
        orientation: Orientation,
        ratio: f64,
        first: Box<LayoutPlan>,
        second: Box<LayoutPlan>,
    },
}

#[derive(Debug, Clone)]
struct Node {
    parent: Option<Idx>,
    kind: NodeKind,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Pane {
        id: PaneId,
    },
    Split {
        orientation: Orientation,
        ratio: f64,
        first: Idx,
        second: Idx,
    },
}

// ── LayoutTree ────────────────────────────────────────────────────────────────

/// Mutable binary tree of panes and split containers for a single tab (§18).
///
/// Create with [`LayoutTree::new`], which produces a tree with one pane
/// (`PaneId(1)`). Use [`split_right`](Self::split_right) /
/// [`split_down`](Self::split_down) to add panes and
/// [`close_pane`](Self::close_pane) to remove them.
#[derive(Debug, Clone)]
pub struct LayoutTree {
    nodes: Vec<Option<Node>>,
    root: Idx,
    focus: PaneId,
    zoomed: Option<PaneId>,
    next_pane_id: u64,
    pane_index: HashMap<PaneId, Idx>,
    free_list: Vec<Idx>,
}

impl LayoutTree {
    /// Create a tree with a single pane (`PaneId(1)`), focused.
    pub fn new() -> Self {
        let id = PaneId(1);
        let mut pane_index = HashMap::new();
        pane_index.insert(id.clone(), 0);
        Self {
            nodes: vec![Some(Node {
                parent: None,
                kind: NodeKind::Pane { id: id.clone() },
            })],
            root: 0,
            focus: id,
            zoomed: None,
            next_pane_id: 2,
            pane_index,
            free_list: Vec::new(),
        }
    }

    /// Build a tree from a workspace definition [`PaneNode`].
    ///
    /// Returns the tree and a list of `(pane_name, PaneId)` mappings in
    /// depth-first order. The first pane is focused by default.
    pub fn from_pane_node(node: &PaneNode) -> (Self, Vec<(String, PaneId)>) {
        let mut tree = Self {
            nodes: Vec::new(),
            root: 0,
            focus: PaneId(0), // placeholder — set below
            zoomed: None,
            next_pane_id: 1,
            pane_index: HashMap::new(),
            free_list: Vec::new(),
        };
        let mut mappings = Vec::new();
        let root_idx = tree.build_from_node(node, &mut mappings);
        tree.root = root_idx;
        if let Some((_, ref id)) = mappings.first() {
            tree.focus = id.clone();
        }
        (tree, mappings)
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Currently focused pane.
    pub fn focus(&self) -> PaneId {
        self.focus.clone()
    }

    /// Number of panes in the tree.
    pub fn pane_count(&self) -> usize {
        self.pane_index.len()
    }

    /// All pane IDs in depth-first (left-to-right) order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::with_capacity(self.pane_index.len());
        self.collect_panes(self.root, &mut out);
        out
    }

    /// Whether a pane is currently zoomed.
    pub fn is_zoomed(&self) -> bool {
        self.zoomed.is_some()
    }

    /// The zoomed pane, if any.
    pub fn zoomed_pane(&self) -> Option<PaneId> {
        self.zoomed.clone()
    }

    /// Compute bounding rectangles for every visible pane given the total tab
    /// area. When a pane is zoomed only that pane is returned, filling the
    /// entire area.
    pub fn compute_rects(&self, total: Rect) -> HashMap<PaneId, Rect> {
        let mut out = HashMap::new();
        if self.pane_index.is_empty() {
            return out;
        }
        if let Some(ref z) = self.zoomed {
            out.insert(z.clone(), total);
            return out;
        }
        self.rects_recursive(self.root, total, &mut out);
        out
    }

    /// Reconstruct a [`PaneNode`] tree from this layout, using `leaf_fn` to
    /// populate each leaf's name and session definition.
    pub fn to_pane_node<F>(&self, leaf_fn: F) -> PaneNode
    where
        F: Fn(&PaneId) -> PaneLeaf,
    {
        self.node_to_pane_node(self.root, &leaf_fn)
    }

    /// Reassign every pane ID in the tree using the provided allocator.
    ///
    /// Returns a map from old pane ID to new pane ID. The tree focus, zoomed
    /// pane, pane index, and next pane counter are updated to match.
    pub fn reassign_pane_ids<F>(&mut self, mut alloc: F) -> HashMap<PaneId, PaneId>
    where
        F: FnMut() -> PaneId,
    {
        let mut mapping = HashMap::new();
        let mut max_assigned = 0u64;

        for node in self.nodes.iter_mut().flatten() {
            if let NodeKind::Pane { id } = &mut node.kind {
                let old = id.clone();
                let new = alloc();
                max_assigned = max_assigned.max(new.0);
                *id = new.clone();
                mapping.insert(old, new);
            }
        }

        let old_focus = self.focus.clone();
        if let Some(new_focus) = mapping.get(&old_focus).cloned() {
            self.focus = new_focus;
        }
        self.zoomed = self
            .zoomed
            .take()
            .and_then(|old| mapping.get(&old).cloned());

        self.pane_index.clear();
        for (idx, node) in self.nodes.iter().enumerate() {
            if let Some(Node {
                kind: NodeKind::Pane { id },
                ..
            }) = node
            {
                self.pane_index.insert(id.clone(), idx);
            }
        }

        if max_assigned > 0 {
            self.next_pane_id = max_assigned + 1;
        }

        mapping
    }

    // ── Split ─────────────────────────────────────────────────────────────────

    /// Split the target pane horizontally (left/right). The original pane
    /// becomes the left child; a new pane is created on the right (§18.5).
    pub fn split_right(&mut self, target: PaneId) -> Result<PaneId, LayoutError> {
        self.split(target, Orientation::Horizontal)
    }

    /// Split the target pane vertically (top/bottom). The original pane becomes
    /// the top child; a new pane is created below (§18.5).
    pub fn split_down(&mut self, target: PaneId) -> Result<PaneId, LayoutError> {
        self.split(target, Orientation::Vertical)
    }

    // ── Close ─────────────────────────────────────────────────────────────────

    /// Remove a pane from the tree, collapsing its parent split (§18.6).
    pub fn close_pane(&mut self, target: PaneId) -> Result<CloseResult, LayoutError> {
        let target_idx = self.pane_idx(target.clone())?;

        if self.zoomed.as_ref() == Some(&target) {
            self.zoomed = None;
        }

        let parent_idx = match self.node(target_idx).parent {
            Some(p) => p,
            None => {
                self.pane_index.remove(&target);
                self.free_node(target_idx);
                return Ok(CloseResult::LastClosed);
            }
        };

        // Identify sibling.
        let sibling_idx = match &self.node(parent_idx).kind {
            NodeKind::Split { first, second, .. } => {
                if *first == target_idx {
                    *second
                } else {
                    *first
                }
            }
            _ => unreachable!(),
        };

        let grandparent = self.node(parent_idx).parent;

        // Promote sibling into parent's slot (preserves root/grandparent refs).
        let sibling_node = self.nodes[sibling_idx].take().unwrap();
        self.nodes[parent_idx] = Some(sibling_node);
        self.node_mut(parent_idx).parent = grandparent;

        // Fix child parent pointers when the promoted node is a split.
        if let NodeKind::Split { first, second, .. } = &self.node(parent_idx).kind {
            let (f, s) = (*first, *second);
            self.node_mut(f).parent = Some(parent_idx);
            self.node_mut(s).parent = Some(parent_idx);
        }

        // Fix pane_index when the promoted node is a leaf.
        if let NodeKind::Pane { ref id } = self.node(parent_idx).kind {
            self.pane_index.insert(id.clone(), parent_idx);
        }

        // Free removed slots.
        self.pane_index.remove(&target);
        self.free_node(target_idx);
        self.free_list.push(sibling_idx); // already None from .take()

        // Move focus when the closed pane was focused.
        let new_focus = if self.focus == target {
            let f = self.leftmost_pane(parent_idx);
            self.focus = f.clone();
            f
        } else {
            self.focus.clone()
        };

        Ok(CloseResult::Closed { new_focus })
    }

    // ── Focus ─────────────────────────────────────────────────────────────────

    /// Set focus to a specific pane.
    pub fn set_focus(&mut self, target: PaneId) -> Result<(), LayoutError> {
        if !self.pane_index.contains_key(&target) {
            return Err(LayoutError::PaneNotFound(target));
        }
        self.focus = target;
        Ok(())
    }

    /// Move focus to the next pane in depth-first order, wrapping (§18.7).
    pub fn focus_next(&mut self) {
        let panes = self.panes();
        if let Some(pos) = panes.iter().position(|p| *p == self.focus) {
            self.focus = panes[(pos + 1) % panes.len()].clone();
        }
    }

    /// Move focus to the previous pane in depth-first order, wrapping (§18.7).
    pub fn focus_prev(&mut self) {
        let panes = self.panes();
        if let Some(pos) = panes.iter().position(|p| *p == self.focus) {
            let prev = if pos == 0 { panes.len() - 1 } else { pos - 1 };
            self.focus = panes[prev].clone();
        }
    }

    /// Move focus to the nearest pane in the given direction using geometric
    /// centres (§18.7). No-op if no pane exists in that direction.
    pub fn focus_direction(&mut self, dir: Direction, total: Rect) {
        if let Some(id) = self.nearest_pane_in_direction(&self.focus, dir, total) {
            self.focus = id;
        }
    }

    /// Swap the target pane with its nearest spatial neighbor in the given
    /// direction. Pane IDs are preserved and therefore move with their attached
    /// sessions and focus.
    pub fn swap_pane_with_neighbor(
        &mut self,
        target: PaneId,
        dir: Direction,
        total: Rect,
    ) -> Result<(), LayoutError> {
        let target_idx = self.pane_idx(target.clone())?;
        let Some(other) = self.nearest_pane_in_direction(&target, dir, total) else {
            return Ok(());
        };
        let other_idx = self.pane_idx(other.clone())?;

        if target_idx == other_idx {
            return Ok(());
        }

        {
            let target_node = self.node_mut(target_idx);
            match &mut target_node.kind {
                NodeKind::Pane { id } => *id = other.clone(),
                NodeKind::Split { .. } => unreachable!(),
            }
        }
        {
            let other_node = self.node_mut(other_idx);
            match &mut other_node.kind {
                NodeKind::Pane { id } => *id = target.clone(),
                NodeKind::Split { .. } => unreachable!(),
            }
        }

        self.pane_index.insert(target, other_idx);
        self.pane_index.insert(other, target_idx);
        Ok(())
    }

    // ── Zoom ──────────────────────────────────────────────────────────────────

    /// Toggle zoom for the focused pane (§18.8).
    pub fn toggle_zoom(&mut self) {
        if self.zoomed.is_some() {
            self.zoomed = None;
        } else {
            self.zoomed = Some(self.focus.clone());
        }
    }

    // ── Structural transforms ────────────────────────────────────────────────

    /// Toggle the orientation of the nearest ancestor split containing the
    /// target pane.
    pub fn toggle_split_orientation(&mut self, target: PaneId) -> Result<(), LayoutError> {
        let target_idx = self.pane_idx(target)?;
        let Some(split_idx) = self.nearest_ancestor_any_split(target_idx) else {
            return Ok(());
        };

        if let NodeKind::Split { orientation, .. } = &mut self.node_mut(split_idx).kind {
            *orientation = match orientation {
                Orientation::Horizontal => Orientation::Vertical,
                Orientation::Vertical => Orientation::Horizontal,
            };
        }
        Ok(())
    }

    /// Set the nearest ancestor split containing the target pane to an even
    /// 50/50 ratio.
    pub fn equalize_pane_split(&mut self, target: PaneId) -> Result<(), LayoutError> {
        let target_idx = self.pane_idx(target)?;
        let Some(split_idx) = self.nearest_ancestor_any_split(target_idx) else {
            return Ok(());
        };

        if let NodeKind::Split { ratio, .. } = &mut self.node_mut(split_idx).kind {
            *ratio = 0.5;
        }
        Ok(())
    }

    /// Reset every split in the tab layout to an even 50/50 ratio.
    pub fn equalize_tab(&mut self) {
        self.equalize_subtree(self.root);
    }

    /// Rebuild the layout as an even left-to-right arrangement using the
    /// current stable pane order.
    pub fn retile_even_horizontal(&mut self) {
        let panes = self.panes();
        self.rebuild_from_plan(Self::build_even_plan(&panes, Orientation::Horizontal));
    }

    /// Rebuild the layout as an even top-to-bottom arrangement using the
    /// current stable pane order.
    pub fn retile_even_vertical(&mut self) {
        let panes = self.panes();
        self.rebuild_from_plan(Self::build_even_plan(&panes, Orientation::Vertical));
    }

    /// Rebuild the layout as a near-square grid using the current stable pane
    /// order.
    pub fn retile_grid(&mut self) {
        let panes = self.panes();
        self.rebuild_from_plan(Self::build_grid_plan(&panes));
    }

    // ── Resize ────────────────────────────────────────────────────────────────

    /// Resize the target pane by `cells` character cells in the given direction
    /// (§18.9). Adjusts the ratio of the nearest ancestor split in the relevant
    /// orientation, clamped to minimum pane sizes.
    pub fn resize_pane(
        &mut self,
        target: PaneId,
        dir: ResizeDirection,
        cells: u16,
        total: Rect,
    ) -> Result<(), LayoutError> {
        let (orient, growing) = match dir {
            ResizeDirection::GrowRight => (Orientation::Horizontal, true),
            ResizeDirection::ShrinkRight => (Orientation::Horizontal, false),
            ResizeDirection::GrowDown => (Orientation::Vertical, true),
            ResizeDirection::ShrinkDown => (Orientation::Vertical, false),
        };
        self.resize_against_orientation(target, orient, growing, cells, total)
    }

    /// Move the nearest relevant splitter in the given arrow-key direction,
    /// independent of whether the focused pane is the first or second child of
    /// that split. This matches Windows Terminal-style Alt+Shift+Arrow
    /// semantics.
    pub fn resize_pane_toward(
        &mut self,
        target: PaneId,
        dir: Direction,
        cells: u16,
        total: Rect,
    ) -> Result<(), LayoutError> {
        let target_idx = self.pane_idx(target.clone())?;
        let Some((_split_idx, in_first, orient)) = (match dir {
            Direction::Left | Direction::Right => self
                .nearest_ancestor_split(target_idx, Orientation::Horizontal)
                .map(|(split_idx, in_first)| (split_idx, in_first, Orientation::Horizontal)),
            Direction::Up | Direction::Down => self
                .nearest_ancestor_split(target_idx, Orientation::Vertical)
                .map(|(split_idx, in_first)| (split_idx, in_first, Orientation::Vertical)),
        }) else {
            return Ok(());
        };

        let growing = match (dir, orient.clone(), in_first) {
            (Direction::Right, Orientation::Horizontal, true) => true,
            (Direction::Right, Orientation::Horizontal, false) => false,
            (Direction::Left, Orientation::Horizontal, true) => false,
            (Direction::Left, Orientation::Horizontal, false) => true,
            (Direction::Down, Orientation::Vertical, true) => true,
            (Direction::Down, Orientation::Vertical, false) => false,
            (Direction::Up, Orientation::Vertical, true) => false,
            (Direction::Up, Orientation::Vertical, false) => true,
            _ => unreachable!(),
        };

        self.resize_against_orientation(target, orient, growing, cells, total)
    }

    fn resize_against_orientation(
        &mut self,
        target: PaneId,
        orient: Orientation,
        growing: bool,
        cells: u16,
        total: Rect,
    ) -> Result<(), LayoutError> {
        let target_idx = self.pane_idx(target)?;
        let Some((split_idx, in_first)) = self.nearest_ancestor_split(target_idx, orient.clone())
        else {
            return Ok(());
        };

        let split_rect = self.node_rect(split_idx, total);
        let total_dim = match orient {
            Orientation::Horizontal => split_rect.width,
            Orientation::Vertical => split_rect.height,
        };
        if total_dim == 0 {
            return Ok(());
        }

        let delta = cells as f64 / total_dim as f64;

        let (old_ratio, first_idx, second_idx) = match &self.node(split_idx).kind {
            NodeKind::Split {
                ratio,
                first,
                second,
                ..
            } => (*ratio, *first, *second),
            _ => unreachable!(),
        };

        let new_ratio = if (in_first && growing) || (!in_first && !growing) {
            old_ratio + delta
        } else {
            old_ratio - delta
        };

        let min_first = self.min_dim(first_idx, &orient) as f64 / total_dim as f64;
        let max_ratio = 1.0 - self.min_dim(second_idx, &orient) as f64 / total_dim as f64;
        let lo = min_first.max(0.1);
        let hi = max_ratio.min(0.9);
        if lo > hi {
            return Ok(());
        }
        let clamped = new_ratio.clamp(lo, hi);

        if let NodeKind::Split { ratio, .. } = &mut self.node_mut(split_idx).kind {
            *ratio = clamped;
        }
        Ok(())
    }

    fn nearest_ancestor_split(&self, target_idx: Idx, orient: Orientation) -> Option<(Idx, bool)> {
        let mut cur = target_idx;
        loop {
            let p = self.node(cur).parent?;
            if let NodeKind::Split {
                orientation, first, ..
            } = &self.node(p).kind
            {
                let in_first = *first == cur;
                if *orientation == orient {
                    return Some((p, in_first));
                }
            }
            cur = p;
        }
    }

    fn nearest_ancestor_any_split(&self, target_idx: Idx) -> Option<Idx> {
        let mut cur = target_idx;
        loop {
            let p = self.node(cur).parent?;
            if matches!(self.node(p).kind, NodeKind::Split { .. }) {
                return Some(p);
            }
            cur = p;
        }
    }

    fn nearest_pane_in_direction(
        &self,
        target: &PaneId,
        dir: Direction,
        total: Rect,
    ) -> Option<PaneId> {
        let rects = self.compute_rects(total);
        let cur = rects.get(target)?;
        let (cx, cy) = (cur.center_x(), cur.center_y());

        let mut best: Option<(PaneId, f64)> = None;
        for (id, rect) in &rects {
            if id == target {
                continue;
            }
            let (px, py) = (rect.center_x(), rect.center_y());
            let in_dir = match dir {
                Direction::Up => py < cy,
                Direction::Down => py > cy,
                Direction::Left => px < cx,
                Direction::Right => px > cx,
            };
            if !in_dir {
                continue;
            }
            let dist = (px - cx).powi(2) + (py - cy).powi(2);
            if best.as_ref().map_or(true, |(_, d)| dist < *d) {
                best = Some((id.clone(), dist));
            }
        }
        best.map(|(id, _)| id)
    }

    // ── Internals ─────────────────────────────────────────────────────────────

    fn pane_idx(&self, id: PaneId) -> Result<Idx, LayoutError> {
        self.pane_index
            .get(&id)
            .copied()
            .ok_or(LayoutError::PaneNotFound(id))
    }

    fn node(&self, idx: Idx) -> &Node {
        self.nodes[idx].as_ref().expect("dangling node index")
    }

    fn node_mut(&mut self, idx: Idx) -> &mut Node {
        self.nodes[idx].as_mut().expect("dangling node index")
    }

    fn alloc_node(&mut self, node: Node) -> Idx {
        if let Some(idx) = self.free_list.pop() {
            self.nodes[idx] = Some(node);
            idx
        } else {
            let idx = self.nodes.len();
            self.nodes.push(Some(node));
            idx
        }
    }

    fn free_node(&mut self, idx: Idx) {
        self.nodes[idx] = None;
        self.free_list.push(idx);
    }

    fn equalize_subtree(&mut self, idx: Idx) {
        let children = match &mut self.node_mut(idx).kind {
            NodeKind::Pane { .. } => return,
            NodeKind::Split {
                ratio,
                first,
                second,
                ..
            } => {
                *ratio = 0.5;
                (*first, *second)
            }
        };

        self.equalize_subtree(children.0);
        self.equalize_subtree(children.1);
    }

    fn rebuild_from_plan(&mut self, plan: LayoutPlan) {
        let focus = self.focus.clone();
        let zoomed = self.zoomed.clone();
        let next_pane_id = self.next_pane_id;

        self.nodes.clear();
        self.pane_index.clear();
        self.free_list.clear();
        self.root = self.build_from_plan(plan, None);
        self.focus = focus;
        self.zoomed = zoomed;
        self.next_pane_id = next_pane_id;
    }

    fn build_from_plan(&mut self, plan: LayoutPlan, parent: Option<Idx>) -> Idx {
        match plan {
            LayoutPlan::Pane(id) => {
                let idx = self.alloc_node(Node {
                    parent,
                    kind: NodeKind::Pane { id: id.clone() },
                });
                self.pane_index.insert(id, idx);
                idx
            }
            LayoutPlan::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                let idx = self.alloc_node(Node {
                    parent,
                    kind: NodeKind::Split {
                        orientation,
                        ratio,
                        first: 0,
                        second: 0,
                    },
                });
                let first_idx = self.build_from_plan(*first, Some(idx));
                let second_idx = self.build_from_plan(*second, Some(idx));
                if let NodeKind::Split { first, second, .. } = &mut self.node_mut(idx).kind {
                    *first = first_idx;
                    *second = second_idx;
                }
                idx
            }
        }
    }

    fn build_even_plan(panes: &[PaneId], orientation: Orientation) -> LayoutPlan {
        match panes.len() {
            0 => unreachable!("retile requires at least one pane"),
            1 => LayoutPlan::Pane(panes[0].clone()),
            len => {
                let mid = len / 2;
                LayoutPlan::Split {
                    orientation: orientation.clone(),
                    ratio: mid as f64 / len as f64,
                    first: Box::new(Self::build_even_plan(&panes[..mid], orientation.clone())),
                    second: Box::new(Self::build_even_plan(&panes[mid..], orientation)),
                }
            }
        }
    }

    fn build_grid_plan(panes: &[PaneId]) -> LayoutPlan {
        match panes.len() {
            0 => unreachable!("retile requires at least one pane"),
            1 => LayoutPlan::Pane(panes[0].clone()),
            len => {
                let cols = (len as f64).sqrt().ceil() as usize;
                let row_plans: Vec<LayoutPlan> = panes
                    .chunks(cols)
                    .map(|row| Self::build_even_plan(row, Orientation::Horizontal))
                    .collect();
                Self::build_plan_from_nodes(&row_plans, Orientation::Vertical)
            }
        }
    }

    fn build_plan_from_nodes(nodes: &[LayoutPlan], orientation: Orientation) -> LayoutPlan {
        match nodes.len() {
            0 => unreachable!("retile requires at least one node"),
            1 => nodes[0].clone(),
            len => {
                let mid = len / 2;
                LayoutPlan::Split {
                    orientation: orientation.clone(),
                    ratio: mid as f64 / len as f64,
                    first: Box::new(Self::build_plan_from_nodes(
                        &nodes[..mid],
                        orientation.clone(),
                    )),
                    second: Box::new(Self::build_plan_from_nodes(&nodes[mid..], orientation)),
                }
            }
        }
    }

    fn alloc_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane_id);
        self.next_pane_id += 1;
        id
    }

    fn node_to_pane_node<F>(&self, idx: Idx, leaf_fn: &F) -> PaneNode
    where
        F: Fn(&PaneId) -> PaneLeaf,
    {
        match &self.node(idx).kind {
            NodeKind::Pane { id } => PaneNode::Pane(leaf_fn(id)),
            NodeKind::Split {
                orientation,
                ratio,
                first,
                second,
            } => PaneNode::Split(SplitNode {
                orientation: orientation.clone(),
                ratio: Some(*ratio),
                children: vec![
                    self.node_to_pane_node(*first, leaf_fn),
                    self.node_to_pane_node(*second, leaf_fn),
                ],
            }),
        }
    }

    fn build_from_node(&mut self, node: &PaneNode, mappings: &mut Vec<(String, PaneId)>) -> Idx {
        match node {
            PaneNode::Pane(leaf) => {
                let id = self.alloc_pane_id();
                let idx = self.alloc_node(Node {
                    parent: None,
                    kind: NodeKind::Pane { id: id.clone() },
                });
                self.pane_index.insert(id.clone(), idx);
                mappings.push((leaf.name.clone(), id));
                idx
            }
            PaneNode::Split(SplitNode {
                orientation,
                ratio,
                children,
            }) => {
                let first_idx = self.build_from_node(&children[0], mappings);
                let second_idx = self.build_from_node(&children[1], mappings);
                let split_idx = self.alloc_node(Node {
                    parent: None,
                    kind: NodeKind::Split {
                        orientation: orientation.clone(),
                        ratio: ratio.unwrap_or(0.5),
                        first: first_idx,
                        second: second_idx,
                    },
                });
                self.node_mut(first_idx).parent = Some(split_idx);
                self.node_mut(second_idx).parent = Some(split_idx);
                split_idx
            }
        }
    }

    fn split(&mut self, target: PaneId, orientation: Orientation) -> Result<PaneId, LayoutError> {
        let target_idx = self.pane_idx(target.clone())?;
        let target_parent = self.node(target_idx).parent;

        let new_id = self.alloc_pane_id();
        let new_pane_idx = self.alloc_node(Node {
            parent: None,
            kind: NodeKind::Pane { id: new_id.clone() },
        });

        // Move original pane out of its slot to a fresh slot.
        let original = self.nodes[target_idx].take().unwrap();
        let original_idx = self.alloc_node(original);
        self.pane_index.insert(target, original_idx);

        // Parent both children under the new split.
        self.node_mut(original_idx).parent = Some(target_idx);
        self.node_mut(new_pane_idx).parent = Some(target_idx);

        // Place split node in the original slot (preserves root/parent refs).
        self.nodes[target_idx] = Some(Node {
            parent: target_parent,
            kind: NodeKind::Split {
                orientation,
                ratio: 0.5,
                first: original_idx,
                second: new_pane_idx,
            },
        });

        self.pane_index.insert(new_id.clone(), new_pane_idx);
        Ok(new_id)
    }

    fn collect_panes(&self, idx: Idx, out: &mut Vec<PaneId>) {
        match &self.node(idx).kind {
            NodeKind::Pane { id } => out.push(id.clone()),
            NodeKind::Split { first, second, .. } => {
                self.collect_panes(*first, out);
                self.collect_panes(*second, out);
            }
        }
    }

    fn leftmost_pane(&self, idx: Idx) -> PaneId {
        match &self.node(idx).kind {
            NodeKind::Pane { id } => id.clone(),
            NodeKind::Split { first, .. } => self.leftmost_pane(*first),
        }
    }

    fn rects_recursive(&self, idx: Idx, area: Rect, out: &mut HashMap<PaneId, Rect>) {
        match &self.node(idx).kind {
            NodeKind::Pane { id } => {
                out.insert(id.clone(), area);
            }
            NodeKind::Split {
                orientation,
                ratio,
                first,
                second,
            } => {
                let (a, b) = Self::divide(area, orientation, *ratio);
                self.rects_recursive(*first, a, out);
                self.rects_recursive(*second, b, out);
            }
        }
    }

    fn divide(area: Rect, orientation: &Orientation, ratio: f64) -> (Rect, Rect) {
        match orientation {
            Orientation::Horizontal => {
                let w1 = (area.width as f64 * ratio).round() as u16;
                let w2 = area.width.saturating_sub(w1);
                (
                    Rect::new(area.x, area.y, w1, area.height),
                    Rect::new(area.x + w1, area.y, w2, area.height),
                )
            }
            Orientation::Vertical => {
                let h1 = (area.height as f64 * ratio).round() as u16;
                let h2 = area.height.saturating_sub(h1);
                (
                    Rect::new(area.x, area.y, area.width, h1),
                    Rect::new(area.x, area.y + h1, area.width, h2),
                )
            }
        }
    }

    /// Compute the bounding rect for a specific node by walking from root.
    fn node_rect(&self, target: Idx, total: Rect) -> Rect {
        let mut path = vec![target];
        let mut cur = target;
        while cur != self.root {
            cur = self.node(cur).parent.expect("broken parent chain");
            path.push(cur);
        }
        path.reverse(); // [root, ..., target]

        let mut area = total;
        for i in 0..path.len().saturating_sub(1) {
            if let NodeKind::Split {
                orientation,
                ratio,
                first,
                ..
            } = &self.node(path[i]).kind
            {
                let (a, b) = Self::divide(area, orientation, *ratio);
                area = if *first == path[i + 1] { a } else { b };
            }
        }
        area
    }

    /// Minimum cells a subtree requires along `orientation`.
    fn min_dim(&self, idx: Idx, orientation: &Orientation) -> u16 {
        match &self.node(idx).kind {
            NodeKind::Pane { .. } => match orientation {
                Orientation::Horizontal => MIN_PANE_COLS,
                Orientation::Vertical => MIN_PANE_ROWS,
            },
            NodeKind::Split {
                orientation: so,
                first,
                second,
                ..
            } => {
                let a = self.min_dim(*first, orientation);
                let b = self.min_dim(*second, orientation);
                if so == orientation {
                    a + b
                } else {
                    a.max(b)
                }
            }
        }
    }
}

impl Default for LayoutTree {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    // -- Split tests -------------------------------------------------------

    #[test]
    fn split_right_creates_two_panes() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();

        assert_eq!(tree.pane_count(), 2);
        assert_eq!(tree.panes(), vec![p1.clone(), p2.clone()]);

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 40, 24));
        assert_eq!(rects[&p2], Rect::new(40, 0, 40, 24));
    }

    #[test]
    fn split_down_creates_two_panes() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_down(p1.clone()).unwrap();

        assert_eq!(tree.pane_count(), 2);
        assert_eq!(tree.panes(), vec![p1.clone(), p2.clone()]);

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 80, 12));
        assert_eq!(rects[&p2], Rect::new(0, 12, 80, 12));
    }

    #[test]
    fn split_right_then_down() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();
        let p3 = tree.split_down(p1.clone()).unwrap();

        assert_eq!(tree.pane_count(), 3);
        assert_eq!(tree.panes(), vec![p1.clone(), p3.clone(), p2.clone()]);

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 40, 12));
        assert_eq!(rects[&p3], Rect::new(0, 12, 40, 12));
        assert_eq!(rects[&p2], Rect::new(40, 0, 40, 24));
    }

    #[test]
    fn split_nonexistent_pane() {
        let mut tree = LayoutTree::new();
        assert_eq!(
            tree.split_right(PaneId(999)).unwrap_err(),
            LayoutError::PaneNotFound(PaneId(999))
        );
    }

    // -- Close tests -------------------------------------------------------

    #[test]
    fn close_promotes_sibling_leaf() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();

        let result = tree.close_pane(p1).unwrap();
        assert_eq!(
            result,
            CloseResult::Closed {
                new_focus: p2.clone()
            }
        );
        assert_eq!(tree.pane_count(), 1);
        assert_eq!(tree.panes(), vec![p2]);
    }

    #[test]
    fn close_promotes_sibling_subtree() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();
        let p3 = tree.split_down(p1.clone()).unwrap();

        // Close p2 — the V-split subtree (p1, p3) becomes root.
        let result = tree.close_pane(p2).unwrap();
        assert_eq!(
            result,
            CloseResult::Closed {
                new_focus: p1.clone()
            }
        );
        assert_eq!(tree.pane_count(), 2);
        assert_eq!(tree.panes(), vec![p1.clone(), p3.clone()]);

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 80, 12));
        assert_eq!(rects[&p3], Rect::new(0, 12, 80, 12));
    }

    #[test]
    fn close_last_pane() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        assert_eq!(tree.close_pane(p1).unwrap(), CloseResult::LastClosed);
        assert_eq!(tree.pane_count(), 0);
    }

    #[test]
    fn close_focused_pane_moves_focus() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();
        let _p3 = tree.split_down(p2.clone()).unwrap();

        tree.set_focus(p2.clone()).unwrap();
        let result = tree.close_pane(p2).unwrap();

        // Focus should move to the leftmost pane of the promoted sibling.
        if let CloseResult::Closed { new_focus } = result {
            assert!(tree.panes().contains(&new_focus));
            assert_eq!(tree.focus(), new_focus);
        } else {
            panic!("expected Closed");
        }
    }

    // -- Resize tests ------------------------------------------------------

    #[test]
    fn resize_adjusts_ratio() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();

        tree.resize_pane(p1.clone(), ResizeDirection::GrowRight, 8, area())
            .unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1].width, 48); // 0.6 * 80
        assert_eq!(rects[&p2].width, 32);
    }

    #[test]
    fn resize_clamps_at_min_size() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();

        tree.resize_pane(p1.clone(), ResizeDirection::GrowRight, 200, area())
            .unwrap();

        let rects = tree.compute_rects(area());
        // Ratio clamped to 0.9 → p1=72, p2=8. Both >= MIN_PANE_COLS.
        assert_eq!(rects[&p1].width, 72);
        assert_eq!(rects[&p2].width, 8);
        assert!(rects[&p2].width >= MIN_PANE_COLS);
    }

    #[test]
    fn resize_clamps_with_nested_splits() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();
        let _p3 = tree.split_right(p2.clone()).unwrap();

        // In a 10-col area, the second child subtree (H-split of p2+p3)
        // needs min 2+2=4 cols, so max ratio = 1 - 4/10 = 0.6.
        let small = Rect::new(0, 0, 10, 4);
        tree.resize_pane(p1.clone(), ResizeDirection::GrowRight, 100, small)
            .unwrap();

        let rects = tree.compute_rects(small);
        assert_eq!(rects[&p1].width, 6); // 0.6 * 10
    }

    #[test]
    fn resize_noop_without_relevant_split() {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let _p2 = tree.split_right(p1.clone()).unwrap();

        // GrowDown looks for a vertical split, but tree only has horizontal.
        tree.resize_pane(p1.clone(), ResizeDirection::GrowDown, 4, area())
            .unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 40, 24)); // unchanged
    }

    #[test]
    fn resize_toward_right_moves_splitter_right_even_from_right_pane() {
        let mut tree = LayoutTree::new();
        let left = tree.focus();
        let right = tree.split_right(left.clone()).unwrap();

        tree.resize_pane_toward(right.clone(), Direction::Right, 8, area())
            .unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&left].width, 48);
        assert_eq!(rects[&right].width, 32);
    }

    #[test]
    fn resize_toward_left_moves_splitter_left_even_from_left_pane() {
        let mut tree = LayoutTree::new();
        let left = tree.focus();
        let right = tree.split_right(left.clone()).unwrap();

        tree.resize_pane_toward(left.clone(), Direction::Left, 8, area())
            .unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&left].width, 32);
        assert_eq!(rects[&right].width, 48);
    }

    #[test]
    fn resize_toward_up_moves_splitter_up_even_from_bottom_pane() {
        let mut tree = LayoutTree::new();
        let top = tree.focus();
        let bottom = tree.split_down(top.clone()).unwrap();

        tree.resize_pane_toward(bottom.clone(), Direction::Up, 4, area())
            .unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&top].height, 8);
        assert_eq!(rects[&bottom].height, 16);
    }

    #[test]
    fn toggle_split_orientation_flips_nearest_ancestor_split() {
        let mut tree = LayoutTree::new();
        let left = tree.focus();
        let right = tree.split_right(left.clone()).unwrap();

        tree.toggle_split_orientation(right.clone()).unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&left], Rect::new(0, 0, 80, 12));
        assert_eq!(rects[&right], Rect::new(0, 12, 80, 12));
    }

    #[test]
    fn equalize_pane_split_resets_nearest_ratio_to_half() {
        let mut tree = LayoutTree::new();
        let left = tree.focus();
        let right = tree.split_right(left.clone()).unwrap();
        tree.resize_pane(left.clone(), ResizeDirection::GrowRight, 16, area())
            .unwrap();

        tree.equalize_pane_split(right.clone()).unwrap();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&left].width, 40);
        assert_eq!(rects[&right].width, 40);
    }

    #[test]
    fn equalize_tab_resets_nested_split_ratios() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        tree.resize_pane(p1.clone(), ResizeDirection::GrowRight, 16, area())
            .unwrap();
        tree.resize_pane(p2.clone(), ResizeDirection::GrowDown, 6, area())
            .unwrap();

        tree.equalize_tab();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 40, 12));
        assert_eq!(rects[&p3], Rect::new(0, 12, 40, 12));
        assert_eq!(rects[&p2], Rect::new(40, 0, 40, 12));
        assert_eq!(rects[&p4], Rect::new(40, 12, 40, 12));
    }

    #[test]
    fn retile_even_horizontal_rebuilds_stable_left_to_right_layout() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        tree.set_focus(p3.clone()).unwrap();

        tree.retile_even_horizontal();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 20, 24));
        assert_eq!(rects[&p3], Rect::new(20, 0, 20, 24));
        assert_eq!(rects[&p2], Rect::new(40, 0, 20, 24));
        assert_eq!(rects[&p4], Rect::new(60, 0, 20, 24));
        assert_eq!(tree.focus(), p3);
    }

    #[test]
    fn retile_even_vertical_rebuilds_stable_top_to_bottom_layout() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        tree.set_focus(p2.clone()).unwrap();

        tree.retile_even_vertical();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 80, 6));
        assert_eq!(rects[&p3], Rect::new(0, 6, 80, 6));
        assert_eq!(rects[&p2], Rect::new(0, 12, 80, 6));
        assert_eq!(rects[&p4], Rect::new(0, 18, 80, 6));
        assert_eq!(tree.focus(), p2);
    }

    #[test]
    fn retile_grid_rebuilds_four_panes_as_even_grid() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        tree.set_focus(p4.clone()).unwrap();

        tree.retile_grid();

        let rects = tree.compute_rects(area());
        assert_eq!(rects[&p1], Rect::new(0, 0, 40, 12));
        assert_eq!(rects[&p3], Rect::new(40, 0, 40, 12));
        assert_eq!(rects[&p2], Rect::new(0, 12, 40, 12));
        assert_eq!(rects[&p4], Rect::new(40, 12, 40, 12));
        assert_eq!(tree.focus(), p4);
    }

    // -- Focus traversal tests ---------------------------------------------

    fn four_pane_tree() -> (LayoutTree, PaneId, PaneId, PaneId, PaneId) {
        let mut tree = LayoutTree::new();
        let p1 = tree.focus();
        let p2 = tree.split_right(p1.clone()).unwrap();
        let p3 = tree.split_down(p1.clone()).unwrap();
        let p4 = tree.split_down(p2.clone()).unwrap();
        (tree, p1, p2, p3, p4)
    }

    #[test]
    fn swap_pane_right_exchanges_positions_with_right_neighbor() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        let before = tree.compute_rects(area());

        tree.set_focus(p1.clone()).unwrap();
        tree.swap_pane_with_neighbor(p1.clone(), Direction::Right, area())
            .unwrap();

        let after = tree.compute_rects(area());
        assert_eq!(after[&p1], before[&p2]);
        assert_eq!(after[&p2], before[&p1]);
        assert_eq!(after[&p3], before[&p3]);
        assert_eq!(after[&p4], before[&p4]);
        assert_eq!(tree.focus(), p1);
    }

    #[test]
    fn swap_pane_down_exchanges_positions_with_lower_neighbor() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        let before = tree.compute_rects(area());

        tree.set_focus(p2.clone()).unwrap();
        tree.swap_pane_with_neighbor(p2.clone(), Direction::Down, area())
            .unwrap();

        let after = tree.compute_rects(area());
        assert_eq!(after[&p2], before[&p4]);
        assert_eq!(after[&p4], before[&p2]);
        assert_eq!(after[&p1], before[&p1]);
        assert_eq!(after[&p3], before[&p3]);
        assert_eq!(tree.focus(), p2);
    }

    #[test]
    fn swap_pane_noop_without_neighbor_in_direction() {
        let (mut tree, p1, _p2, _p3, _p4) = four_pane_tree();
        let before = tree.compute_rects(area());

        tree.swap_pane_with_neighbor(p1.clone(), Direction::Up, area())
            .unwrap();

        assert_eq!(tree.compute_rects(area()), before);
        assert_eq!(tree.focus(), p1);
    }

    #[test]
    fn focus_next_wraps() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        // DFS order: p1, p3, p2, p4
        assert_eq!(tree.focus(), p1);

        tree.focus_next();
        assert_eq!(tree.focus(), p3);

        tree.focus_next();
        assert_eq!(tree.focus(), p2);

        tree.focus_next();
        assert_eq!(tree.focus(), p4);

        tree.focus_next();
        assert_eq!(tree.focus(), p1); // wrap
    }

    #[test]
    fn focus_prev_wraps() {
        let (mut tree, p1, _p2, _p3, p4) = four_pane_tree();
        assert_eq!(tree.focus(), p1);

        tree.focus_prev();
        assert_eq!(tree.focus(), p4); // wrap to last
    }

    #[test]
    fn focus_direction_all_four() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();
        let a = area();

        // p1 is top-left → right → p2 (top-right)
        tree.set_focus(p1.clone()).unwrap();
        tree.focus_direction(Direction::Right, a);
        assert_eq!(tree.focus(), p2);

        // p2 → down → p4
        tree.focus_direction(Direction::Down, a);
        assert_eq!(tree.focus(), p4);

        // p4 → left → p3
        tree.focus_direction(Direction::Left, a);
        assert_eq!(tree.focus(), p3);

        // p3 → up → p1
        tree.focus_direction(Direction::Up, a);
        assert_eq!(tree.focus(), p1);
    }

    #[test]
    fn focus_direction_noop_at_edge() {
        let (mut tree, p1, _p2, _p3, _p4) = four_pane_tree();
        let a = area();

        tree.set_focus(p1.clone()).unwrap();
        tree.focus_direction(Direction::Up, a);
        assert_eq!(tree.focus(), p1);

        tree.focus_direction(Direction::Left, a);
        assert_eq!(tree.focus(), p1);
    }

    // -- Zoom tests --------------------------------------------------------

    #[test]
    fn zoom_unzoom_preserves_layout() {
        let (mut tree, p1, _p2, _p3, _p4) = four_pane_tree();
        let a = area();

        let before = tree.compute_rects(a);

        tree.set_focus(p1.clone()).unwrap();
        tree.toggle_zoom();
        assert!(tree.is_zoomed());
        assert_eq!(tree.zoomed_pane(), Some(p1.clone()));

        let zoomed_rects = tree.compute_rects(a);
        assert_eq!(zoomed_rects.len(), 1);
        assert_eq!(zoomed_rects[&p1], a);

        tree.toggle_zoom();
        assert!(!tree.is_zoomed());

        let after = tree.compute_rects(a);
        assert_eq!(before, after);
    }

    #[test]
    fn zoom_preserves_all_panes() {
        let (mut tree, p1, p2, p3, p4) = four_pane_tree();

        tree.set_focus(p1.clone()).unwrap();
        tree.toggle_zoom();

        assert_eq!(tree.pane_count(), 4);
        let panes = tree.panes();
        assert!(panes.contains(&p1));
        assert!(panes.contains(&p2));
        assert!(panes.contains(&p3));
        assert!(panes.contains(&p4));
    }
}
