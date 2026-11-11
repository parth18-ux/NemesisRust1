//! Publish payments to partitioned Redis Streams: partition = crc32(payer) mod N, so one worker owns each payer.
use rusetect::payment::Payment;

pub fn partition_of(p: &Payment, n: u32) -> u32 {
    crc32fast::hash(format!("{}|{}", p.from_bank, p.from_account).as_bytes()) % n
}

pub fn publish(con: &mut redis::Connection, payments: &[Payment], partitions: u32, prefix: &str, chunk: usize, stop: bool) -> redis::RedisResult<usize> {
    let mut k = 0;
    for part in payments.chunks(chunk.max(1)) {
        let mut pl = redis::pipe();
        for p in part {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
            pl.cmd("XADD").arg(format!("{prefix}payments:{}", partition_of(p, partitions))).arg("*")
                .arg("p").arg(serde_json::to_string(p).unwrap()).arg("enq_ns").arg(now).ignore();
            k += 1;
        }
        pl.query::<()>(con)?;
    }
    if stop {
        for i in 0..partitions {
            redis::cmd("XADD").arg(format!("{prefix}payments:{i}")).arg("*").arg("stop").arg(1).query::<()>(con)?;
        }
    }
    Ok(k)
}
