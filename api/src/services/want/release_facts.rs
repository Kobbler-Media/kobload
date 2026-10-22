use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::parsing::{ParsedRelease, parse_release};
use crate::quality::{ParsedQuality, parse_quality};
use crate::trash_filters::TrashFilterSet;
use crate::watch::Release;
use crate::watch::matcher::PreparedTitle;

/// What scoring needs from a release that doesn't depend on the pattern,
/// computed on first use. One per release per fetch, shared by every pattern
/// that scores it.
pub struct ReleaseFacts<'r> {
    release: &'r Arc<Release>,
    title: PreparedTitle<'r>,
    quality: OnceCell<ParsedQuality>,
    parsed: RefCell<HashMap<&'static str, Rc<ParsedRelease>>>,
    blocklist: OnceCell<(*const TrashFilterSet, Option<String>)>,
}

impl<'r> ReleaseFacts<'r> {
    pub fn new(release: &'r Arc<Release>) -> Self {
        Self {
            release,
            title: PreparedTitle::new(&release.title),
            quality: OnceCell::new(),
            parsed: RefCell::default(),
            blocklist: OnceCell::new(),
        }
    }

    pub fn release(&self) -> &'r Release {
        self.release
    }

    pub fn shared(&self) -> Arc<Release> {
        Arc::clone(self.release)
    }

    pub fn title(&self) -> &PreparedTitle<'r> {
        &self.title
    }

    pub fn quality(&self) -> &ParsedQuality {
        self.quality
            .get_or_init(|| parse_quality(&self.release.title))
    }

    pub fn parsed(&self, media_kind: &'static str) -> Rc<ParsedRelease> {
        self.parsed
            .borrow_mut()
            .entry(media_kind)
            .or_insert_with(|| Rc::new(parse_release(&self.release.title, media_kind)))
            .clone()
    }

    /// The TRaSH blocklist category for this release. Cached for the first
    /// filter set it's asked about; a different set is checked uncached.
    pub fn blocklist(&self, filters: &TrashFilterSet, group: Option<&str>) -> Option<String> {
        let check = || {
            filters
                .check_blocklist(&self.release.title, group)
                .map(String::from)
        };
        let (cached_for, category) = self
            .blocklist
            .get_or_init(|| (std::ptr::from_ref(filters), check()));
        if std::ptr::eq(*cached_for, filters) {
            category.clone()
        } else {
            check()
        }
    }
}
