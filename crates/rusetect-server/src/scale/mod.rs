//! Bank scale streaming (port of service/scale/): account state in Redis, payments on partitioned Redis Streams,
//! stateless scoring workers. Partition = crc32(payer) mod N, so every payer's payments are handled in order by one
//! worker; a payee's receipts from other partitions may lag by milliseconds (the usual streaming trade off).
pub mod producer;
pub mod redis_state;
pub mod worker;
