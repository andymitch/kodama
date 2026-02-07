//! Wire format for command messages over QUIC streams
//!
//! Each command message is sent as:
//! - 4 bytes: payload length (big-endian u32)
//! - N bytes: MessagePack-encoded CommandMessage

use anyhow::Result;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use kodama_core::{CommandMessage, ClientCommandMessage, MAX_COMMAND_SIZE};

/// Write a command message to an async writer (length-prefixed MessagePack)
pub async fn write_command<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &CommandMessage,
) -> Result<()> {
    let bytes = rmp_serde::to_vec(msg)?;

    if bytes.len() > MAX_COMMAND_SIZE {
        anyhow::bail!("Command message too large: {} > {}", bytes.len(), MAX_COMMAND_SIZE);
    }

    writer.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    writer.write_all(&bytes).await?;

    Ok(())
}

/// Read a command message from an async reader (length-prefixed MessagePack)
pub async fn read_command<R: AsyncRead + Unpin>(reader: &mut R) -> Result<CommandMessage> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;

    if len > MAX_COMMAND_SIZE {
        anyhow::bail!("Command message too large: {} > {}", len, MAX_COMMAND_SIZE);
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;

    Ok(rmp_serde::from_slice(&buf)?)
}

/// Write a client command message (targeted) to an async writer
pub async fn write_client_command<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &ClientCommandMessage,
) -> Result<()> {
    let bytes = rmp_serde::to_vec(msg)?;

    if bytes.len() > MAX_COMMAND_SIZE {
        anyhow::bail!("Client command message too large: {} > {}", bytes.len(), MAX_COMMAND_SIZE);
    }

    writer.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    writer.write_all(&bytes).await?;

    Ok(())
}

/// Read a client command message (targeted) from an async reader
pub async fn read_client_command<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<ClientCommandMessage> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;

    if len > MAX_COMMAND_SIZE {
        anyhow::bail!("Client command message too large: {} > {}", len, MAX_COMMAND_SIZE);
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;

    Ok(rmp_serde::from_slice(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kodama_core::{CommandRequest, CommandResponse, Command, CommandResult};

    #[tokio::test]
    async fn test_command_write_read_roundtrip() {
        let msg = CommandMessage::Request(CommandRequest {
            id: 1,
            command: Command::RequestStatus,
        });

        let mut buf = Vec::new();
        write_command(&mut buf, &msg).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_command(&mut cursor).await.unwrap();

        match decoded {
            CommandMessage::Request(req) => {
                assert_eq!(req.id, 1);
                assert!(matches!(req.command, Command::RequestStatus));
            }
            _ => panic!("Expected Request"),
        }
    }

    #[tokio::test]
    async fn test_response_write_read_roundtrip() {
        let msg = CommandMessage::Response(CommandResponse {
            id: 99,
            result: CommandResult::Ok,
        });

        let mut buf = Vec::new();
        write_command(&mut buf, &msg).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_command(&mut cursor).await.unwrap();

        match decoded {
            CommandMessage::Response(resp) => {
                assert_eq!(resp.id, 99);
                assert!(matches!(resp.result, CommandResult::Ok));
            }
            _ => panic!("Expected Response"),
        }
    }
}
