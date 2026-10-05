use crate::containers::{MemberFile, import_members, member_dir, write_member};
use crate::email::{Email, parse_email, split_mbox};
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::Read;

pub struct EmailImporter;

const EMAIL_SNIFF: ProbeScore = ProbeScore(250);
const SNIFF_BYTES: u64 = 4 * 1024;

fn is_mbox(prefix: &[u8]) -> bool {
    prefix.starts_with(b"From ")
}

/// Whether a prefix opens with an RFC 5322 header block naming a sender.
fn looks_like_email(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let mut lines = text.lines();
    if is_mbox(prefix) {
        lines.next();
    }
    let mut from = false;
    let mut other = false;
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            return false;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return false;
        }
        match name.to_ascii_lowercase().as_str() {
            "from" => from = true,
            "subject" | "date" | "message-id" | "received" | "to" => other = true,
            _ => {}
        }
    }
    from && other
}

fn extension(source: &SourceRecord) -> String {
    source
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn email_unit(source_id: Uuid, email: &Email) -> Unit {
    let mut unit = Unit::text(source_id, email.to_text());
    let meta = [
        ("email.subject", email.subject.clone()),
        ("email.from", Some(email.from.join(", "))),
        ("email.date", email.date.clone()),
        ("email.message-id", email.message_id.clone()),
    ];
    for (key, value) in meta {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            unit.metadata.insert(key.into(), value);
        }
    }
    unit
}

impl Plugin for EmailImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "email".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["eml".into(), "mbox".into()],
            mime_types: vec!["message/rfc822".into(), "application/mbox".into()],
            priority: 50,
        }
    }
}

impl Importer for EmailImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if self.descriptor().extensions.contains(&extension(source)) {
            return ProbeScore::EXTENSION;
        }
        let mut prefix = Vec::new();
        let read =
            fs::File::open(&source.path).and_then(|f| f.take(SNIFF_BYTES).read_to_end(&mut prefix));
        if read.is_ok() && looks_like_email(&prefix) {
            EMAIL_SNIFF
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        self.import_with_members(ctx, source, &NoMembers)
    }

    fn import_with_members(
        &self,
        ctx: &JobContext,
        mut source: SourceRecord,
        members: &dyn MemberImporter,
    ) -> Result<ImportOutcome> {
        let raw =
            fs::read(&source.path).with_context(|| format!("read {}", source.path.display()))?;
        let name = anytopdf_core::basename(&source.path);
        let mut warnings = Vec::new();
        let messages: Vec<(String, Email)> = if extension(&source) == "mbox" || is_mbox(&raw) {
            let mut parsed = Vec::new();
            for (i, raw) in split_mbox(&raw)?.iter().enumerate() {
                match parse_email(raw) {
                    Ok(email) => parsed.push((format!("message {}", i + 1), email)),
                    Err(e) => warnings.push(
                        Diagnostic::new(
                            DiagnosticCode::MembersNotImported,
                            format!("{name}: message {} was not imported: {e:#}", i + 1),
                        )
                        .to_string(),
                    ),
                }
            }
            anyhow::ensure!(!parsed.is_empty(), "no email messages found");
            parsed
        } else {
            let email = parse_email(&raw)?;
            for (key, value) in [
                ("email.subject", email.subject.clone()),
                ("email.date", email.date.clone()),
            ] {
                if let Some(value) = value {
                    source.metadata.insert(key.into(), value);
                }
            }
            vec![(String::new(), email)]
        };

        let several = messages.len() > 1;
        let mut units = Vec::new();
        for (label, email) in &messages {
            let mut unit = email_unit(source.id, email);
            if several {
                unit.metadata
                    .insert("container.member".into(), label.clone());
            }
            units.push(unit);
            if email.dropped_attachments > 0 {
                warnings.push(
                    Diagnostic::new(
                        DiagnosticCode::MembersNotImported,
                        format!(
                            "{name}: {} attachments beyond the first {} were not imported",
                            email.dropped_attachments,
                            crate::email::MAX_ATTACHMENTS
                        ),
                    )
                    .to_string(),
                );
            }
            if email.attachments.is_empty() {
                continue;
            }
            let dir = member_dir(ctx, "email")?;
            let mut files = Vec::new();
            for (i, attachment) in email.attachments.iter().enumerate() {
                let label = if several {
                    format!("{label} / {}", attachment.filename)
                } else {
                    attachment.filename.clone()
                };
                if let Err(e) = members.charge(attachment.data.len() as u64) {
                    warnings.push(
                        Diagnostic::new(
                            DiagnosticCode::MembersNotImported,
                            format!("{name}: {label} was not imported: {e:#}"),
                        )
                        .to_string(),
                    );
                    continue;
                }
                let path = write_member(&dir, i, &attachment.filename, &attachment.data)?;
                files.push(MemberFile { label, path });
            }
            let (member_units, member_warnings) = import_members(ctx, members, &source, files);
            units.extend(member_units);
            warnings.extend(member_warnings);
        }

        Ok(ImportOutcome {
            source,
            units,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinOptions, register_builtins};
    use std::path::Path;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> SourceRecord {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        SourceRecord::new(path)
    }

    fn builtins() -> Registry {
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        registry
    }

    const WITH_ATTACHMENTS: &[u8] = b"From: a@example.com\r\nTo: b@example.com\r\n\
Subject: Receipts\r\nContent-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nTwo receipts attached.\r\n\
--x\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; filename=\"../r1.txt\"\r\n\r\nreceipt one\r\n\
--x\r\nContent-Type: application/x-unknown\r\nContent-Disposition: attachment; filename=\"blob.bin\"\r\n\r\n\x00\x01\x02\r\n\
--x--\r\n";

    #[test]
    fn pipeline_imports_body_and_attachments_through_registered_importers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mail.eml");
        fs::write(&path, WITH_ATTACHMENTS).unwrap();
        let run = Pipeline::new(builtins()).ingest(&[path], true).unwrap();
        assert_eq!(run.graph.sources.len(), 1);
        let source = &run.graph.sources[0];
        assert_eq!(source.metadata["email.subject"], "Receipts");
        let texts: Vec<&str> = run
            .graph
            .units
            .iter()
            .map(|u| u.visible_text.as_deref().unwrap_or_default())
            .collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts[0].contains("Subject: Receipts"), "{}", texts[0]);
        assert!(texts[0].contains("Two receipts attached."), "{}", texts[0]);
        assert_eq!(texts[1], "receipt one");
        let member = &run.graph.units[1];
        assert_eq!(member.source_id, source.id);
        assert_eq!(member.metadata["container.member"], "../r1.txt");
        let skipped: Vec<_> = run
            .warnings
            .iter()
            .filter(|d| d.code == DiagnosticCode::MembersNotImported)
            .collect();
        assert_eq!(skipped.len(), 1, "{:?}", run.warnings);
        assert!(
            skipped[0].message.contains("blob.bin"),
            "{}",
            skipped[0].message
        );
        assert!(
            !run.warnings.iter().any(|d| d.code.is_skip()),
            "{:?}",
            run.warnings
        );
        // Attachments land inside the job workspace, never beside the input.
        assert!(!dir.path().join("r1.txt").exists());
    }

    #[test]
    fn standalone_import_reports_attachments_without_a_member_importer() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(dir.path(), "mail.eml", WITH_ATTACHMENTS);
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let outcome = EmailImporter.import(&ctx, source).unwrap();
        assert_eq!(outcome.units.len(), 1);
        assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
    }

    #[test]
    fn mbox_becomes_one_unit_per_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("box.mbox");
        fs::write(
            &path,
            b"From a@example.com Mon Oct  5 09:00:00 2026\nFrom: a@example.com\nSubject: one\n\nfirst\n\n\
From b@example.com Mon Oct  5 10:00:00 2026\nFrom: b@example.com\nSubject: two\n\nsecond\n",
        )
        .unwrap();
        let run = Pipeline::new(builtins()).ingest(&[path], true).unwrap();
        let units = &run.graph.units;
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].metadata["email.subject"], "one");
        assert_eq!(units[1].metadata["container.member"], "message 2");
        assert!(units[1].visible_text.as_deref().unwrap().contains("second"));
    }

    #[test]
    fn nested_messages_recurse_and_stop_at_the_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw = b"From: z@example.com\r\nSubject: level 0\r\n\r\ninnermost\r\n".to_vec();
        for level in 1..=MAX_MEMBER_DEPTH + 1 {
            let mut outer = format!(
                "From: z@example.com\r\nSubject: level {level}\r\n\
Content-Type: multipart/mixed; boundary=b{level}\r\n\r\n\
--b{level}\r\nContent-Type: text/plain\r\n\r\nwrapper {level}\r\n\
--b{level}\r\nContent-Type: message/rfc822\r\n\r\n"
            )
            .into_bytes();
            outer.extend_from_slice(&raw);
            outer.extend_from_slice(format!("\r\n--b{level}--\r\n").as_bytes());
            raw = outer;
        }
        let path = dir.path().join("nested.eml");
        fs::write(&path, raw).unwrap();
        let run = Pipeline::new(builtins()).ingest(&[path], true).unwrap();
        assert_eq!(run.graph.units.len(), MAX_MEMBER_DEPTH + 1);
        assert!(
            run.warnings
                .iter()
                .any(|d| d.code == DiagnosticCode::MembersNotImported
                    && d.message.contains("nested")),
            "{:?}",
            run.warnings
        );
    }

    #[test]
    fn probe_prefers_email_headers_over_text_sniffing() {
        let dir = tempfile::tempdir().unwrap();
        let registry = builtins();
        let eml = write(
            dir.path(),
            "saved-message",
            b"Received: by mx\r\nFrom: a@example.com\r\nSubject: s\r\n\r\nbody",
        );
        let mbox = write(
            dir.path(),
            "Inbox",
            b"From a@example.com Mon Oct  5 09:00:00 2026\nFrom: a@example.com\nDate: x\n\nbody\n",
        );
        let prose = write(dir.path(), "letter", b"From: the desk of Ada\nDear Bob,\n");
        for source in [&eml, &mbox] {
            assert_eq!(
                registry.importer_for(source).unwrap().descriptor().name,
                "email",
                "{}",
                source.path.display()
            );
        }
        assert_eq!(
            registry.importer_for(&prose).unwrap().descriptor().name,
            "text"
        );
    }
}
