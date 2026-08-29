//! Pure protocol projections for the canonical [`FileAttributes`] model.
//!
//! Facades must use these helpers so partial updates never clobber fields
//! belonging to other protocols (read-modify-write of the full record).
//!
//! Pattern for every protocol:
//! - `to_*` copies a **subset** out for getattr/stat.
//! - `merge_from_*` applies a patch of `Option` fields; `None` means unchanged.
//!
//! Never `*attrs = FileAttributes::from(protocol_struct)` — that zeros SMB/macOS
//! fields on a FUSE chmod. Tests in this module are the regression net.
//!
//! Maintainer: this is the enforcement point for "Partial setattr" trap.
//! All setattr paths (FUSE, NFS, SMB, WebDAV, macOS) must go through the
//! merge_from_* helpers. See "Partial setattr", "attr_map", and "canonical
// FileAttributes".

mod fuse;
mod macos;
mod nfs;
mod smb;
mod webdav;

pub use fuse::{merge_from_fuse, to_fuse, FuseSetAttr, FuseStat};
pub use macos::{merge_from_macos, to_macos, MacOsSetAttr, MacOsStat};
pub use nfs::{merge_from_nfs4, to_nfs4, Nfs4Fattr, Nfs4SetAttr};
pub use smb::{merge_from_smb3, to_smb3, Smb3FileInfo, Smb3SetInfo};
pub use webdav::{merge_from_webdav, to_webdav_props, WebDavProperties, WebDavSetProps};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::{DosFlags, FileAttributes, MacOsFlags, NamedStream, Timespec};
    #[allow(unused_imports)]
    use arkfs_test_review::{
        review_assert as assert, review_eq as assert_eq, review_ne as assert_ne,
    };

    /// Shared fixture for projection/merge tests.
    fn rich_attrs() -> FileAttributes {
        let mut a = FileAttributes::new_file(42, 0o640);
        a.uid = 1000;
        a.gid = 1000;
        a.owner_name = Some("alice".into());
        a.group_name = Some("staff".into());
        a.logical_size = 1024;
        a.allocation_size = 4096;
        a.atime = Timespec::new(1, 0);
        a.mtime = Timespec::new(2, 0);
        a.ctime = Timespec::new(3, 0);
        a.btime = Timespec::new(0, 0);
        a.change_attr = 7;
        a.dos = DosFlags {
            hidden: true,
            archive: true,
            system: true,
            ..Default::default()
        };
        a.macos = MacOsFlags {
            uf_hidden: true,
            uf_immutable: true,
            ..Default::default()
        };
        a.xattrs
            .insert("com.apple.quarantine".into(), b"0000;abc".to_vec());
        a.xattrs.insert("user.comment".into(), b"hello".to_vec());
        a.streams.push(NamedStream {
            name: NamedStream::RESOURCE_FORK.into(),
            size: 64,
            content_id: None,
        });
        a.streams.push(NamedStream {
            name: ":Zone.Identifier".into(),
            size: 16,
            content_id: None,
        });
        a.content_type = Some("text/plain".into());
        a.etag = Some("\"v1\"".into());
        a.dead_props
            .insert("{DAV:}displayname".into(), "notes.txt".into());
        a.dead_props.insert("{urn:ex}color".into(), "blue".into());
        a
    }

    /// FUSE setattr must not clear DOS/macOS flags.
    #[test]
    fn fuse_partial_setattr_preserves_smb_and_macos() {
        let _g = arkfs_test_review::guard();
        let mut attrs = rich_attrs();
        let before_dos = attrs.dos;
        let before_macos = attrs.macos;
        let before_xattrs = attrs.xattrs.clone();
        let before_extra_streams: Vec<_> = attrs
            .streams
            .iter()
            .filter(|s| s.name != NamedStream::PRIMARY)
            .cloned()
            .collect();
        let before_dead = attrs.dead_props.clone();

        merge_from_fuse(
            &mut attrs,
            &FuseSetAttr {
                mode: Some(0o600),
                uid: Some(2000),
                gid: None,
                size: Some(512),
                atime: Some(Timespec::new(10, 0)),
                mtime: Some(Timespec::new(11, 0)),
                ctime: None,
            },
            Timespec::new(12, 0),
        );

        assert_eq!(attrs.mode, 0o600);
        assert_eq!(attrs.uid, 2000);
        assert_eq!(attrs.gid, 1000); // untouched
        assert_eq!(attrs.logical_size, 512);
        assert_eq!(attrs.dos, before_dos);
        assert_eq!(attrs.macos, before_macos);
        assert_eq!(attrs.xattrs, before_xattrs);
        let extra_streams: Vec<_> = attrs
            .streams
            .iter()
            .filter(|s| s.name != NamedStream::PRIMARY)
            .cloned()
            .collect();
        assert_eq!(extra_streams, before_extra_streams);
        assert_eq!(attrs.dead_props, before_dead);
        assert_eq!(attrs.change_attr, 8);
    }

    /// SMB to_* then merge_from_* is identity on expressed fields.
    #[test]
    fn smb_round_trip_identity_for_expressed_fields() {
        let _g = arkfs_test_review::guard();
        let attrs = rich_attrs();
        let info = to_smb3(&attrs);
        let mut back = FileAttributes::default();
        // seed with unrelated fields that SMB set-info should not clear when not present
        back.macos.uf_datavault = true;
        back.dead_props.insert("{DAV:}x".into(), "keep".into());

        merge_from_smb3(
            &mut back,
            &Smb3SetInfo {
                creation_time: Some(info.creation_time),
                last_access: Some(info.last_access),
                last_write: Some(info.last_write),
                change_time: Some(info.change_time),
                end_of_file: Some(info.end_of_file),
                allocation_size: Some(info.allocation_size),
                dos: Some(info.dos),
            },
            Timespec::new(99, 0),
        );

        assert_eq!(back.btime, attrs.btime);
        assert_eq!(back.atime, attrs.atime);
        assert_eq!(back.mtime, attrs.mtime);
        assert_eq!(back.ctime, attrs.ctime);
        assert_eq!(back.logical_size, attrs.logical_size);
        assert_eq!(back.allocation_size, attrs.allocation_size);
        assert_eq!(back.dos, attrs.dos);
        assert_eq!(back.file_id, 0); // identity is not a SetInfo field
                                     // non-SMB fields preserved
        assert!(back.macos.uf_datavault);
        assert_eq!(
            back.dead_props.get("{DAV:}x").map(String::as_str),
            Some("keep")
        );
    }

    /// NFSv4 projection/merge keeps mode/owner/times.
    #[test]
    fn nfs4_round_trip_mode_owner_times() {
        let _g = arkfs_test_review::guard();
        let attrs = rich_attrs();
        let fattr = to_nfs4(&attrs);
        let mut back = FileAttributes::new_file(1, 0o777);
        back.dos.hidden = true; // must survive NFS merge
        merge_from_nfs4(
            &mut back,
            &Nfs4SetAttr {
                mode: Some(fattr.mode),
                uid: Some(fattr.uid),
                gid: Some(fattr.gid),
                size: Some(fattr.size),
                owner_name: fattr.owner_name.clone(),
                group_name: fattr.group_name.clone(),
                atime: Some(fattr.atime),
                mtime: Some(fattr.mtime),
                ctime: None,
            },
            Timespec::new(50, 0),
        );
        assert_eq!(back.mode, attrs.mode);
        assert_eq!(back.uid, attrs.uid);
        assert_eq!(back.owner_name, attrs.owner_name);
        assert!(back.dos.hidden);
    }

    /// WebDAV merge must not drop named streams.
    #[test]
    fn webdav_merge_preserves_streams() {
        let _g = arkfs_test_review::guard();
        let mut attrs = rich_attrs();
        let streams = attrs.streams.clone();
        merge_from_webdav(
            &mut attrs,
            &WebDavSetProps {
                content_type: Some("application/json".into()),
                displayname: Some("other".into()),
                dead_props: {
                    let mut m = std::collections::BTreeMap::new();
                    m.insert("{urn:ex}color".into(), "red".into());
                    m
                },
            },
            Timespec::new(1, 0),
        );
        assert_eq!(attrs.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            attrs.dead_props.get("{urn:ex}color").map(String::as_str),
            Some("red")
        );
        assert_eq!(attrs.streams, streams);
        assert!(attrs.dos.hidden);
    }

    /// macOS merge keeps DOS flags and WebDAV dead props.
    #[test]
    fn macos_merge_preserves_dos_and_dead_props() {
        let _g = arkfs_test_review::guard();
        let mut attrs = rich_attrs();
        let dead = attrs.dead_props.clone();
        merge_from_macos(
            &mut attrs,
            &MacOsSetAttr {
                mode: Some(0o755),
                flags: Some(MacOsFlags {
                    uf_hidden: false,
                    uf_immutable: true,
                    uf_compressed: true,
                    ..Default::default()
                }),
                btime: Some(Timespec::new(5, 0)),
                xattrs_set: {
                    let mut m = std::collections::BTreeMap::new();
                    m.insert("com.apple.FinderInfo".into(), vec![0u8; 32]);
                    m
                },
                xattrs_remove: vec![],
            },
            Timespec::new(6, 0),
        );
        assert!(!attrs.macos.uf_hidden);
        assert!(attrs.macos.uf_compressed);
        assert!(attrs.dos.hidden);
        assert_eq!(attrs.dead_props, dead);
        assert!(attrs.xattrs.contains_key("com.apple.FinderInfo"));
        assert!(attrs.xattrs.contains_key("user.comment"));
    }

    /// SMB SetInfo must not assign a new file_id.
    #[test]
    fn smb_setinfo_does_not_rewrite_file_id() {
        let _g = arkfs_test_review::guard();
        let mut attrs = FileAttributes::new_file(42, 0o644);
        merge_from_smb3(
            &mut attrs,
            &Smb3SetInfo {
                end_of_file: Some(8),
                ..Default::default()
            },
            Timespec::new(1, 0),
        );
        assert_eq!(attrs.file_id, 42);
        assert_eq!(attrs.logical_size, 8);
    }

    /// to_fuse copies size/nlink/mode and S_IFMT.
    #[test]
    fn fuse_stat_projection() {
        let _g = arkfs_test_review::guard();
        let attrs = rich_attrs();
        let st = to_fuse(&attrs);
        assert_eq!(st.ino, 42);
        assert_eq!(st.mode & 0o7777, 0o640);
        assert_eq!(st.mode & 0o170000, 0o100000); // regular file
        assert_eq!(st.size, 1024);
        assert_eq!(st.nlink, 1);
    }

    /// to_webdav exposes live/dead properties.
    #[test]
    fn webdav_props_projection() {
        let _g = arkfs_test_review::guard();
        let attrs = rich_attrs();
        let p = to_webdav_props(&attrs);
        assert_eq!(p.getcontentlength, 1024);
        assert_eq!(p.getcontenttype.as_deref(), Some("text/plain"));
        assert!(p.dead_props.contains_key("{urn:ex}color"));
    }
}
