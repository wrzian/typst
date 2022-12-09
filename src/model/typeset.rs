use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroUsize;

use comemo::{Track, Tracked, TrackedMut};

use super::{Content, Selector, StyleChain, Value};
use crate::diag::SourceResult;
use crate::doc::{Document, Element, Frame, Location, Meta};
use crate::geom::Transform;
use crate::util::hash128;
use crate::World;

/// Typeset content into a fully layouted document.
#[comemo::memoize]
pub fn typeset(world: Tracked<dyn World>, content: &Content) -> SourceResult<Document> {
    let library = world.library();
    let styles = StyleChain::new(&library.styles);

    let mut document;
    let mut iter = 0;
    let mut introspector = Introspector::new();

    // Relayout until all introspections stabilize.
    // If that doesn't happen within five attempts, we give up.
    loop {
        let mut provider = StabilityProvider::new();
        let mut vt = Vt {
            world,
            provider: provider.track_mut(),
            introspector: introspector.track(),
        };

        document = (library.items.layout)(&mut vt, content, styles)?;
        iter += 1;

        if iter >= 5 || introspector.update(&document) {
            break;
        }
    }

    Ok(document)
}

/// A virtual typesetter.
///
/// Holds the state needed to [typeset] content. This is the equivalent to the
/// [Vm](super::Vm) for typesetting.
pub struct Vt<'a> {
    /// The compilation environment.
    #[doc(hidden)]
    pub world: Tracked<'a, dyn World>,
    /// Provides stable identities to nodes.
    #[doc(hidden)]
    pub provider: TrackedMut<'a, StabilityProvider>,
    /// Provides access to information about the document.
    #[doc(hidden)]
    pub introspector: Tracked<'a, Introspector>,
}

impl<'a> Vt<'a> {
    /// Access the underlying world.
    pub fn world(&self) -> Tracked<'a, dyn World> {
        self.world
    }

    /// Produce a stable identifier for this call site.
    ///
    /// The key should be something that identifies the call site, but is not
    /// necessarily unique. The stable marker incorporates the key's hash plus
    /// additional disambiguation from other call sites with the same key.
    ///
    /// The returned id can be attached to content as metadata is the then
    /// locatable through [`locate`](Self::locate).
    pub fn identify<T: Hash>(&mut self, key: &T) -> StableId {
        self.provider.identify(hash128(key))
    }

    /// Locate all metadata matches for the given selector.
    pub fn locate(&self, selector: Selector) -> Vec<(StableId, &Content)> {
        self.introspector.locate(selector)
    }
}

/// Stably identifies a call site across multiple layout passes.
///
/// This struct is created by [`Vt::identify`].
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct StableId(u128, u64);

/// Provides stable identities to nodes.
#[derive(Clone)]
#[doc(hidden)]
pub struct StabilityProvider(HashMap<u128, u64>);

impl StabilityProvider {
    /// Create a new stability provider.
    fn new() -> Self {
        Self(HashMap::new())
    }
}

#[comemo::track]
impl StabilityProvider {
    /// Produce a stable identifier for this call site.
    fn identify(&mut self, hash: u128) -> StableId {
        let slot = self.0.entry(hash).or_default();
        let id = StableId(hash, *slot);
        *slot += 1;
        id
    }
}

/// Provides access to information about the document.
#[doc(hidden)]
pub struct Introspector {
    nodes: Vec<(StableId, Content)>,
    queries: RefCell<Vec<(Selector, u128)>>,
}

impl Introspector {
    /// Create a new introspector.
    fn new() -> Self {
        Self { nodes: vec![], queries: RefCell::new(vec![]) }
    }

    /// Update the information given new frames and return whether we can stop
    /// layouting.
    fn update(&mut self, document: &Document) -> bool {
        self.nodes.clear();

        for (i, frame) in document.pages.iter().enumerate() {
            let page = NonZeroUsize::new(1 + i).unwrap();
            self.extract(frame, page, Transform::identity());
        }

        let queries = std::mem::take(&mut self.queries).into_inner();
        for (selector, hash) in queries {
            let nodes = self.locate_impl(&selector);
            if hash128(&nodes) != hash {
                return false;
            }
        }

        true
    }

    /// Extract metadata from a frame.
    fn extract(&mut self, frame: &Frame, page: NonZeroUsize, ts: Transform) {
        for (pos, element) in frame.elements() {
            match *element {
                Element::Group(ref group) => {
                    let ts = ts
                        .pre_concat(Transform::translate(pos.x, pos.y))
                        .pre_concat(group.transform);
                    self.extract(&group.frame, page, ts);
                }
                Element::Meta(Meta::Node(id, ref content), _) => {
                    if !self.nodes.iter().any(|&(prev, _)| prev == id) {
                        let pos = pos.transform(ts);
                        let mut node = content.clone();
                        let loc = Location { page, pos };
                        node.push_field("loc", Value::Dict(loc.encode()));
                        self.nodes.push((id, node));
                    }
                }
                _ => {}
            }
        }
    }
}

#[comemo::track]
impl Introspector {
    /// Locate all metadata matches for the given selector.
    fn locate(&self, selector: Selector) -> Vec<(StableId, &Content)> {
        let nodes = self.locate_impl(&selector);
        let mut queries = self.queries.borrow_mut();
        if !queries.iter().any(|(prev, _)| prev == &selector) {
            queries.push((selector, hash128(&nodes)));
        }
        nodes
    }
}

impl Introspector {
    fn locate_impl(&self, selector: &Selector) -> Vec<(StableId, &Content)> {
        self.nodes
            .iter()
            .map(|(id, node)| (*id, node))
            .filter(|(_, target)| selector.matches(target))
            .collect()
    }
}