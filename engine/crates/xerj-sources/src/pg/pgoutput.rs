//! Decoder for `pgoutput`, Postgres' built-in logical decoding plugin,
//! protocol version 1 (Postgres 10+). The message layout is the one in the
//! Postgres docs, "Logical Replication Message Formats".

use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub type_oid: u32,
    /// Part of the replica identity (the primary key, by default).
    pub key: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relation {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    /// A TOASTed value the server did not send because it did not change.
    Unchanged,
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Begin {
        final_lsn: u64,
        commit_ts: i64,
    },
    Commit {
        commit_lsn: u64,
        end_lsn: u64,
        commit_ts: i64,
    },
    Relation(Relation),
    Insert {
        rel: u32,
        new: Vec<Cell>,
    },
    /// `old` is the old key (`K`) or the whole old row (`O`, REPLICA IDENTITY
    /// FULL), present only when the key changed or identity is FULL.
    Update {
        rel: u32,
        old: Option<Vec<Cell>>,
        new: Vec<Cell>,
    },
    Delete {
        rel: u32,
        old: Vec<Cell>,
    },
    Truncate {
        rels: Vec<u32>,
    },
    /// Type, Origin, Message: nothing to apply.
    Other,
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn need(&self, n: usize) -> Result<()> {
        if self.p + n > self.b.len() {
            bail!("truncated pgoutput message");
        }
        Ok(())
    }
    fn u8(&mut self) -> Result<u8> {
        self.need(1)?;
        self.p += 1;
        Ok(self.b[self.p - 1])
    }
    fn u16(&mut self) -> Result<u16> {
        self.need(2)?;
        let v = u16::from_be_bytes([self.b[self.p], self.b[self.p + 1]]);
        self.p += 2;
        Ok(v)
    }
    fn u32(&mut self) -> Result<u32> {
        self.need(4)?;
        let v = u32::from_be_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        Ok(v)
    }
    fn u64(&mut self) -> Result<u64> {
        self.need(8)?;
        let v = u64::from_be_bytes(self.b[self.p..self.p + 8].try_into().unwrap());
        self.p += 8;
        Ok(v)
    }
    fn cstr(&mut self) -> Result<String> {
        let end = self.b[self.p..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| anyhow::anyhow!("unterminated string"))?;
        let s = String::from_utf8_lossy(&self.b[self.p..self.p + end]).into_owned();
        self.p += end + 1;
        Ok(s)
    }
    fn tuple(&mut self) -> Result<Vec<Cell>> {
        let n = self.u16()? as usize;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            match self.u8()? {
                b'n' => out.push(Cell::Null),
                b'u' => out.push(Cell::Unchanged),
                b't' | b'b' => {
                    let len = self.u32()? as usize;
                    self.need(len)?;
                    out.push(Cell::Text(
                        String::from_utf8_lossy(&self.b[self.p..self.p + len]).into_owned(),
                    ));
                    self.p += len;
                }
                k => bail!("unknown tuple cell kind {:?}", k as char),
            }
        }
        Ok(out)
    }
}

pub fn decode(b: &[u8]) -> Result<Msg> {
    let mut c = Cur { b, p: 0 };
    Ok(match c.u8()? {
        b'B' => {
            let final_lsn = c.u64()?;
            let commit_ts = c.u64()? as i64;
            let _xid = c.u32()?;
            Msg::Begin {
                final_lsn,
                commit_ts,
            }
        }
        b'C' => {
            let _flags = c.u8()?;
            let commit_lsn = c.u64()?;
            let end_lsn = c.u64()?;
            let commit_ts = c.u64()? as i64;
            Msg::Commit {
                commit_lsn,
                end_lsn,
                commit_ts,
            }
        }
        b'R' => {
            let oid = c.u32()?;
            let schema = c.cstr()?;
            let name = c.cstr()?;
            let _identity = c.u8()?;
            let n = c.u16()? as usize;
            let mut columns = Vec::with_capacity(n);
            for _ in 0..n {
                let flags = c.u8()?;
                let cname = c.cstr()?;
                let type_oid = c.u32()?;
                let _typmod = c.u32()?;
                columns.push(Column {
                    name: cname,
                    type_oid,
                    key: flags & 1 == 1,
                });
            }
            Msg::Relation(Relation {
                oid,
                schema,
                name,
                columns,
            })
        }
        b'I' => {
            let rel = c.u32()?;
            let _n = c.u8()?;
            Msg::Insert {
                rel,
                new: c.tuple()?,
            }
        }
        b'U' => {
            let rel = c.u32()?;
            let mut kind = c.u8()?;
            let mut old = None;
            if kind == b'K' || kind == b'O' {
                old = Some(c.tuple()?);
                kind = c.u8()?;
            }
            if kind != b'N' {
                bail!("update without a new tuple");
            }
            Msg::Update {
                rel,
                old,
                new: c.tuple()?,
            }
        }
        b'D' => {
            let rel = c.u32()?;
            let _k = c.u8()?;
            Msg::Delete {
                rel,
                old: c.tuple()?,
            }
        }
        b'T' => {
            let n = c.u32()? as usize;
            let _opts = c.u8()?;
            let mut rels = Vec::with_capacity(n);
            for _ in 0..n {
                rels.push(c.u32()?);
            }
            Msg::Truncate { rels }
        }
        _ => Msg::Other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tuple(cells: &[Option<&str>]) -> Vec<u8> {
        let mut b = (cells.len() as u16).to_be_bytes().to_vec();
        for c in cells {
            match c {
                None => b.push(b'n'),
                Some(s) => {
                    b.push(b't');
                    b.extend_from_slice(&(s.len() as u32).to_be_bytes());
                    b.extend_from_slice(s.as_bytes());
                }
            }
        }
        b
    }

    #[test]
    fn relation_insert_update_delete_and_commit_decode() {
        let mut r = vec![b'R'];
        r.extend_from_slice(&16384u32.to_be_bytes());
        r.extend_from_slice(b"public\0orders\0");
        r.push(b'd');
        r.extend_from_slice(&2u16.to_be_bytes());
        r.push(1);
        r.extend_from_slice(b"id\0");
        r.extend_from_slice(&23u32.to_be_bytes());
        r.extend_from_slice(&u32::MAX.to_be_bytes());
        r.push(0);
        r.extend_from_slice(b"note\0");
        r.extend_from_slice(&25u32.to_be_bytes());
        r.extend_from_slice(&u32::MAX.to_be_bytes());
        let Msg::Relation(rel) = decode(&r).unwrap() else {
            panic!()
        };
        assert_eq!(rel.name, "orders");
        assert!(rel.columns[0].key && !rel.columns[1].key);

        let mut i = vec![b'I'];
        i.extend_from_slice(&16384u32.to_be_bytes());
        i.push(b'N');
        i.extend(tuple(&[Some("7"), None]));
        assert_eq!(
            decode(&i).unwrap(),
            Msg::Insert {
                rel: 16384,
                new: vec![Cell::Text("7".into()), Cell::Null]
            }
        );

        let mut u = vec![b'U'];
        u.extend_from_slice(&16384u32.to_be_bytes());
        u.push(b'K');
        u.extend(tuple(&[Some("7"), None]));
        u.push(b'N');
        u.extend(tuple(&[Some("8"), Some("x")]));
        let Msg::Update {
            old: Some(old),
            new,
            ..
        } = decode(&u).unwrap()
        else {
            panic!()
        };
        assert_eq!(old[0], Cell::Text("7".into()));
        assert_eq!(new[1], Cell::Text("x".into()));

        let mut d = vec![b'D'];
        d.extend_from_slice(&16384u32.to_be_bytes());
        d.push(b'K');
        d.extend(tuple(&[Some("8"), None]));
        assert!(matches!(decode(&d).unwrap(), Msg::Delete { .. }));

        let mut c = vec![b'C', 0];
        c.extend_from_slice(&10u64.to_be_bytes());
        c.extend_from_slice(&20u64.to_be_bytes());
        c.extend_from_slice(&5u64.to_be_bytes());
        assert_eq!(
            decode(&c).unwrap(),
            Msg::Commit {
                commit_lsn: 10,
                end_lsn: 20,
                commit_ts: 5
            }
        );
    }

    #[test]
    fn a_truncated_message_is_an_error_not_a_panic() {
        assert!(decode(&[b'I', 0, 0]).is_err());
    }
}
