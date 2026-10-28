use anyhow::{Context, Result};
use futures::{TryStreamExt, stream};
use sqlx::PgConnection;

use crate::model::{School, Source, Target};

#[tracing::instrument(skip(connection, schools))]
pub(super) async fn save_all(
    connection: &mut PgConnection,
    target: &Target,
    schools: &[School],
    url: &str,
) -> Result<()> {
    stream::iter(schools.iter().map(Ok::<_, anyhow::Error>))
        .try_fold(connection, |connection, school| async move {
            save(connection, target, school, url).await?;
            Ok(connection)
        })
        .await
        .map(|_| ())
}

#[tracing::instrument(skip(connection, school))]
async fn save(
    connection: &mut PgConnection,
    target: &Target,
    school: &School,
    url: &str,
) -> Result<()> {
    let source = target.source;
    let details = serde_json::to_value(school)?;
    let public_email = school.public_email.as_deref();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO schools(source,source_id,name,state,city,address,postal_code,website,public_email,source_url) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,NULLIF($9,''),$10) ON CONFLICT(source,source_id) DO UPDATE SET \
         name=EXCLUDED.name,state=COALESCE(EXCLUDED.state,schools.state), \
         city=COALESCE(EXCLUDED.city,schools.city),address=COALESCE(EXCLUDED.address,schools.address), \
         postal_code=COALESCE(EXCLUDED.postal_code,schools.postal_code),website=COALESCE(EXCLUDED.website,schools.website), \
         address_provenance=NULLIF(CASE WHEN EXCLUDED.postal_code IS NOT NULL THEN (CASE WHEN EXCLUDED.address IS NOT NULL THEN COALESCE(schools.address_provenance, '{}'::jsonb) - 'address' ELSE COALESCE(schools.address_provenance, '{}'::jsonb) END) - 'postal_code' WHEN EXCLUDED.address IS NOT NULL THEN COALESCE(schools.address_provenance, '{}'::jsonb) - 'address' ELSE COALESCE(schools.address_provenance, '{}'::jsonb) END, '{}'::jsonb), \
         public_email=COALESCE(EXCLUDED.public_email,schools.public_email),source_url=EXCLUDED.source_url,last_checked=clock_timestamp() RETURNING id",
    ).bind(source.as_str()).bind(&school.id).bind(&school.name).bind(&school.state)
        .bind(&school.city).bind(&school.address).bind(&school.postal_code).bind(&school.website)
        .bind(public_email).bind(url).fetch_one(&mut *connection).await?;
    save_office(connection, id, public_email, url, &target.url, &details).await
}

#[tracing::instrument(skip(connection, email, details))]
async fn save_office(
    connection: &mut PgConnection,
    school: i64,
    email: Option<&str>,
    url: &str,
    checkpoint_url: &str,
    details: &serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO contacts(school_id,kind,role,email,source_url,raw,source_key) \
         SELECT $1,'school_office','',$2,$3,$4 || jsonb_build_object('checkpoint_url',$5::text), \
         encode(sha256(convert_to(jsonb_build_array($3::text,NULL::text,''::text,NULL::text,lower(btrim($2::text)))::text,'UTF8')),'hex') \
         WHERE $2::text IS NOT NULL AND btrim($2) <> '' \
         ON CONFLICT(school_id,kind,source_key) DO UPDATE SET email=EXCLUDED.email, \
         raw=EXCLUDED.raw,last_checked=clock_timestamp()",
    )
    .bind(school)
    .bind(email)
    .bind(url)
    .bind(details)
    .bind(checkpoint_url)
    .execute(connection)
    .await?;
    Ok(())
}

#[tracing::instrument(skip(connection))]
pub(super) async fn find(
    connection: &mut PgConnection,
    source: Source,
    source_id: Option<&str>,
) -> Result<Option<i64>> {
    let Some(source_id) = source_id else {
        return Ok(None);
    };
    sqlx::query_scalar("SELECT id FROM schools WHERE source=$1 AND source_id=$2")
        .bind(source.as_str())
        .bind(source_id)
        .fetch_optional(connection)
        .await?
        .with_context(|| {
            format!(
                "unknown {} school ID {source_id}; import its observed school first",
                source.as_str()
            )
        })
        .map(Some)
}
