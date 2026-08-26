//! WebDAV live properties + dead-prop bag. Dead props are a string map; they
//! are not xattrs. Display name is stored as `{DAV:}displayname` in that map.

use crate::attributes::{FileAttributes, Timespec};
use std::collections::BTreeMap;

/// WebDAV live + dead properties projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebDavProperties {
    pub getcontentlength: u64,
    pub getlastmodified: Timespec,
    pub creationdate: Timespec,
    pub getetag: Option<String>,
    pub getcontenttype: Option<String>,
    pub resourcetype_collection: bool,
    pub displayname: Option<String>,
    pub dead_props: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct WebDavSetProps {
    pub content_type: Option<String>,
    pub displayname: Option<String>,
    /// Merged into dead_props (overwrite keys present; others preserved).
    pub dead_props: BTreeMap<String, String>,
}

/// Project live WebDAV properties. Display name is `{DAV:}displayname` if present.
pub fn to_webdav_props(attrs: &FileAttributes) -> WebDavProperties {
    let displayname = attrs.dead_props.get("{DAV:}displayname").cloned();
    WebDavProperties {
        getcontentlength: attrs.logical_size,
        getlastmodified: attrs.mtime,
        creationdate: attrs.btime,
        getetag: attrs.etag.clone(),
        getcontenttype: attrs.content_type.clone(),
        resourcetype_collection: matches!(attrs.file_type, crate::attributes::FileType::Directory),
        displayname,
        dead_props: attrs.dead_props.clone(),
    }
}

/// Merge WebDAV PROPPATCH. Dead-prop keys in the patch overwrite; others stay.
pub fn merge_from_webdav(attrs: &mut FileAttributes, patch: &WebDavSetProps, now: Timespec) {
    if let Some(ref ct) = patch.content_type {
        attrs.content_type = Some(ct.clone());
    }
    if let Some(ref name) = patch.displayname {
        attrs
            .dead_props
            .insert("{DAV:}displayname".into(), name.clone());
    }
    for (k, v) in &patch.dead_props {
        attrs.dead_props.insert(k.clone(), v.clone());
    }
    attrs.touch_change(now);
}
