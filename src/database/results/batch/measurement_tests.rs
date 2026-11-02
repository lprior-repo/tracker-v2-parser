use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result};
use serde_json::json;

use super::differential_tests::{rows, seed, snapshot, write};
use crate::{durable::pacing::test_support::with_database, model::Source};

#[tokio::test]
#[ignore = "explicit isolated release performance qualification"]
async fn release_result_batching_measurement() -> Result<()> {
    assert!(!cfg!(debug_assertions), "run release qualification");
    let path = PathBuf::from(
        std::env::var("RESULT_BATCH_BENCH_REPORT").context("explicit report path required")?,
    );
    with_database(|pool| Box::pin(async move {
        seed(pool,Source::AthleticNet).await?;
        let mut cases=Vec::new();
        for count in [1,8,32,128] {
            for padding in [256,4096] {
                for replay in [false,true] {
                    let initial=rows(count,padding)?;
                    let mut incoming=initial.clone();
                    if replay {for row in &mut incoming {row.mark="10.90".into();}}
                    let mut serial_us=Vec::new();
                    let mut batch_us=Vec::new();
                    let mut oracle=None;
                    for sample in 0..20 {
                        for serial in if sample%2==0 {[true,false]} else {[false,true]} {
                            let mut transaction=pool.begin().await?;
                            if replay {write(&mut transaction,Source::AthleticNet,&initial,true).await?;}
                            let started=Instant::now();
                            write(&mut transaction,Source::AthleticNet,&incoming,serial).await?;
                            let elapsed=started.elapsed().as_micros();
                            let actual=snapshot(&mut transaction).await?;
                            assert_eq!(actual.len(),count);
                            match &oracle {Some(expected)=>assert_eq!(expected,&actual),None=>oracle=Some(actual)}
                            transaction.rollback().await?;
                            if serial {serial_us.push(elapsed);} else {batch_us.push(elapsed);}
                        }
                    }
                    let median=|values:&[u128]| {let mut sorted=values.to_vec();sorted.sort_unstable();sorted[sorted.len()/2]};
                    cases.push(json!({"rows":count,"padding_bytes_per_row":padding,"replay":replay,"serial_median_us":median(&serial_us),"batch_median_us":median(&batch_us),"serial_us":serial_us,"batch_us":batch_us,"canonical_parity":true}));
                }
            }
        }
        let result=json!({"status":"passed","cases":cases,"scope":"Rust save_all versus exact serial chunk oracle; includes normalization, reference reads, payload encoding and SQL; excludes enclosing commit and wider page effects","samples_per_mode":20,"production_modified":false});
        std::fs::write(&path,serde_json::to_vec_pretty(&result)?)?;
        println!("RESULT_BATCH_QUALIFICATION {}",path.display());
        Ok(())
    })).await
}
