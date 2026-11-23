use super::Level;
use anyhow::{Context, Result, ensure};
use ogg::{reading::PacketReader, writing::{PacketWriteEndInfo, PacketWriter}};
use std::{fs::File, io::{BufReader, BufWriter, Write}, path::Path};
use tempfile::NamedTempFile;

fn u32le(input: &mut &[u8]) -> Result<usize> {
    let (head, tail) = input.split_at_checked(4).context("short Vorbis comment length")?;
    *input = tail;
    Ok(u32::from_le_bytes(head.try_into()?) as usize)
}
fn field<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let length = u32le(input)?;
    let (head, tail) = input.split_at_checked(length).context("short Vorbis comment")?;
    *input = tail;
    Ok(head)
}
fn push(out:&mut Vec<u8>, value:&[u8])->Result<()> {
    out.extend(u32::try_from(value.len())?.to_le_bytes()); out.extend(value); Ok(())
}
fn comments(packet:&[u8], track:Level, album:Level)->Result<Vec<u8>> {
    let mut src=packet.strip_prefix(b"\x03vorbis").context("missing Vorbis comments")?;
    let mut out=b"\x03vorbis".to_vec(); push(&mut out,field(&mut src)?)?;
    let pos=out.len(); out.extend([0;4]); let mut count=0u32;
    for _ in 0..u32le(&mut src)? {
        let value=field(&mut src)?;
        let key=value.split(|b|*b==b'=').next().unwrap_or_default();
        if [b"REPLAYGAIN_TRACK_GAIN".as_slice(),b"REPLAYGAIN_TRACK_PEAK",b"REPLAYGAIN_ALBUM_GAIN",b"REPLAYGAIN_ALBUM_PEAK"].iter().any(|k|key.eq_ignore_ascii_case(k)) {continue;}
        push(&mut out,value)?;count+=1;
    }
    for (kind,l) in [("TRACK",track),("ALBUM",album)] {
        push(&mut out,format!("REPLAYGAIN_{kind}_GAIN={:+.2} dB",l.gain).as_bytes())?;
        push(&mut out,format!("REPLAYGAIN_{kind}_PEAK={:.8}",l.peak).as_bytes())?;
    }
    out[pos..pos+4].copy_from_slice(&count.checked_add(4).context("too many Vorbis comments")?.to_le_bytes());out.push(1);Ok(out)
}

// Stream-copy through FFmpeg can recalculate the final granule and change the
// decoded duration. Copy audio packets with their original page end positions.
pub(super) fn replace(path:&Path,track:Level,album:Level)->Result<()> {
    let mut src=PacketReader::new(BufReader::new(File::open(path)?));
    let mut dest=NamedTempFile::new_in(path.parent().context("missing output directory")?)?;
    let mut buffered=BufWriter::new(dest.as_file_mut());
    let mut out=PacketWriter::new(&mut buffered); let mut packets=0usize;
    while let Some(p)=src.read_packet()? {
        let end=if p.last_in_stream(){PacketWriteEndInfo::EndStream}else if p.last_in_page(){PacketWriteEndInfo::EndPage}else{PacketWriteEndInfo::NormalPacket};
        let granule=p.absgp_page();let serial=p.stream_serial();
        let data=if packets==1{comments(&p.data,track,album)?}else{p.data};
        out.write_packet(data,serial,end,granule)?; packets+=1;
    }
    ensure!(packets>=3,"truncated Vorbis stream");drop(out);buffered.flush()?;drop(buffered);drop(src);dest.persist(path)?;Ok(())
}
