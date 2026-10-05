use std::{path::Path,io};
use tokio::io::{AsyncRead,AsyncReadExt,AsyncWrite,AsyncWriteExt};
use subtle::ConstantTimeEq;
pub const SOCKET_SECRET_BYTES:usize=32;
pub const SOCKET_SECRET_SUFFIX:&str=".secret";
pub const SOCKET_SECRET_FILE_ENV:&str="SENPI_RPC_SOCKET_SECRET_FILE";
pub const SOCKET_HANDSHAKE_TIMEOUT_MS:u64=2000;
pub const WINDOWS_PIPE_PREFIX:&str="\\\\.\\pipe\\senpi-rpc-";
/// `path.win32.normalize` for the forms this transport accepts: separators unified, `.`/`..`
/// resolved, and a drive or UNC prefix preserved.
fn normalize_win32(path:&str)->String{
    let path=path.replace('/',"\\");
    let (prefix,rest)=if path.len()>=2&&path.as_bytes()[1]==b':'{(path[..2].to_owned(),path[2..].to_owned())}
    else if path.starts_with("\\\\"){let trimmed=path.trim_start_matches('\\');let mut parts=trimmed.splitn(3,'\\');let server=parts.next().unwrap_or_default();let share=parts.next().unwrap_or_default();(format!("\\\\{server}\\{share}"),format!("\\{}",parts.next().unwrap_or_default()))}
    else{(String::new(),path.clone())};
    let mut segments:Vec<&str>=Vec::new();
    for segment in rest.split('\\'){match segment{""|"."=>{},".."=>{segments.pop();},other=>segments.push(other)}}
    let joined=segments.join("\\");
    if prefix.is_empty(){joined}else if joined.is_empty(){format!("{prefix}\\")}else{format!("{prefix}\\{joined}")}
}
/// The transport address a logical socket path resolves to: the path itself off Windows, a
/// secret-derived named pipe on Windows. Mirrors senpi's `resolveSocketTransportAddress`.
pub fn resolve_socket_transport_address(socket_path:&str,platform:&str,secret:Option<&[u8]>)->Result<String,String>{
    if platform!="win32"{return Ok(socket_path.into());}
    if socket_path.to_lowercase().starts_with("\\\\.\\pipe\\"){return Ok(socket_path.into());}
    let bytes=socket_path.as_bytes();
    let drive_qualified=bytes.len()>=3&&bytes[0].is_ascii_alphabetic()&&bytes[1]==b':'&&matches!(bytes[2],b'\\'|b'/');
    let unc=socket_path.starts_with("\\\\")&&!socket_path.starts_with("\\\\/");
    if !drive_qualified&&!unc{return Err(format!("Windows RPC socket path must be drive-qualified or UNC: {socket_path}"));}
    let mut identity=normalize_win32(socket_path).to_lowercase().into_bytes();
    if let Some(secret)=secret{identity.extend_from_slice(secret);}
    use sha2::{Digest,Sha256};
    let digest=Sha256::digest(&identity).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    Ok(format!("{WINDOWS_PIPE_PREFIX}{}",digest.get(..32).unwrap_or(&digest)))
}
pub fn socket_secret_path(logical_path:&str)->Result<String,String>{if logical_path.to_lowercase().starts_with("\\\\.\\pipe\\"){return Err(format!("Windows RPC named-pipe addresses need a logical filesystem path for their secret: {logical_path}"));}Ok(format!("{logical_path}{SOCKET_SECRET_SUFFIX}"))}
pub fn read_socket_secret(path:&Path)->io::Result<[u8;SOCKET_SECRET_BYTES]>{std::fs::read(path)?.try_into().map_err(|_|io::Error::new(io::ErrorKind::InvalidData,format!("invalid RPC socket secret: {}",path.display())))}
pub fn create_socket_secret(path:&Path)->io::Result<[u8;SOCKET_SECRET_BYTES]>{
    if let Some(parent)=path.parent(){std::fs::create_dir_all(parent)?;}
    let mut secret=[0;SOCKET_SECRET_BYTES];getrandom::fill(&mut secret).map_err(|error|io::Error::other(error.to_string()))?;
    let mut options=std::fs::OpenOptions::new();options.write(true).create(true).truncate(true);
    #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut file=options.open(path)?;std::io::Write::write_all(&mut file,&secret)?;
    #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;file.set_permissions(std::fs::Permissions::from_mode(0o600))?;}
    Ok(secret)
}
pub fn ensure_socket_secret(path:&Path)->io::Result<[u8;SOCKET_SECRET_BYTES]>{match read_socket_secret(path){Ok(secret)=>Ok(secret),Err(error)if matches!(error.kind(),io::ErrorKind::NotFound|io::ErrorKind::InvalidData)=>create_socket_secret(path),Err(error)=>Err(error)}}
pub async fn authenticate_socket(socket:&mut(impl AsyncRead+Unpin),secret:&[u8])->io::Result<()>{
    let mut received=vec![0;secret.len()];tokio::time::timeout(std::time::Duration::from_millis(SOCKET_HANDSHAKE_TIMEOUT_MS),socket.read_exact(&mut received)).await.map_err(|_|io::Error::new(io::ErrorKind::TimedOut,"RPC socket handshake timed out"))??;
    if bool::from(received.ct_eq(secret)){Ok(())}else{Err(io::Error::new(io::ErrorKind::PermissionDenied,"RPC socket authentication failed"))}
}
pub async fn send_socket_handshake(socket:&mut(impl AsyncWrite+Unpin),secret:&[u8])->io::Result<()>{socket.write_all(secret).await}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn transport_address_passes_through_off_windows_and_validates_win32_paths(){
        assert_eq!(resolve_socket_transport_address("/tmp/rpc.sock","linux",None).unwrap(),"/tmp/rpc.sock");
        assert_eq!(resolve_socket_transport_address("\\\\.\\pipe\\senpi-rpc-abc","win32",None).unwrap(),"\\\\.\\pipe\\senpi-rpc-abc");
        assert!(resolve_socket_transport_address("relative\\socket","win32",None).is_err());
        let name=resolve_socket_transport_address("C:/Agent Dir/rpc.sock","win32",Some(&[7;32])).unwrap();
        assert!(name.starts_with(WINDOWS_PIPE_PREFIX));
        assert_eq!(name.len(),WINDOWS_PIPE_PREFIX.len()+32);
        let same=resolve_socket_transport_address("C:\\Agent Dir\\.\\..\\Agent Dir\\rpc.sock","win32",Some(&[7;32])).unwrap();
        assert_eq!(name,same);
        assert_ne!(name,resolve_socket_transport_address("C:/Agent Dir/rpc.sock","win32",Some(&[8;32])).unwrap());
    }
    #[test]fn existing_secret_is_reused_and_bad_length_replaced(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("rpc.secret");let secret=ensure_socket_secret(&path).unwrap();assert_eq!(ensure_socket_secret(&path).unwrap(),secret);std::fs::write(&path,[0]).unwrap();assert_eq!(ensure_socket_secret(&path).unwrap().len(),32);#[cfg(unix)]{use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode()&0o777,0o600);}}
    #[tokio::test]async fn handshake_leaves_protocol_bytes_unconsumed(){let (mut client,mut server)=tokio::io::duplex(64);let secret=[7;32];send_socket_handshake(&mut client,&secret).await.unwrap();client.write_all(b"{}\n").await.unwrap();authenticate_socket(&mut server,&secret).await.unwrap();let mut line=[0;3];server.read_exact(&mut line).await.unwrap();assert_eq!(&line,b"{}\n");}
    #[tokio::test]async fn wrong_secret_is_rejected(){let (mut client,mut server)=tokio::io::duplex(64);send_socket_handshake(&mut client,&[1;32]).await.unwrap();assert_eq!(authenticate_socket(&mut server,&[2;32]).await.unwrap_err().kind(),io::ErrorKind::PermissionDenied);}
}
