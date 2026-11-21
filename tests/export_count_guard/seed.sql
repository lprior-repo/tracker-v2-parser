WITH athlete AS (
    INSERT INTO athletes(name,graduation_year,state)
    VALUES ('Count Guard ' || $1::text,2028,'MI')
    RETURNING *
), profile AS (
    INSERT INTO athlete_sources(athlete_id,source,source_id,profile_url,name,graduation_year,state)
    SELECT id,'milesplit',$1::text,'https://www.milesplit.com/athletes/' || $1::text,
           name,graduation_year,state
    FROM athlete
    RETURNING *
), owned AS (
    INSERT INTO results(source_result_key,athlete_id,athlete_source_id,source,source_result_id,
                        event,raw_mark,sport,status,is_personal_record,is_relay,source_url,raw)
    SELECT lpad($1::text || ':owned:' || number::text,64,'0'),p.athlete_id,p.id,p.source,
           $1::text || ':owned:' || number::text,'100 Meters','12.34','outdoor','recorded',false,false,
           p.profile_url,jsonb_build_object(
               'athlete_id',p.source_id,'event','100 Meters','mark','12.34','sport','outdoor',
               'result_id',$1::text || ':owned:' || number::text,
               'is_personal_record',false,'is_relay',false)
    FROM profile p CROSS JOIN generate_series(1,$2::integer) number
    RETURNING id
), reviewed AS (
    INSERT INTO results(source_result_key,source,source_result_id,event,raw_mark,sport,status,
                        is_personal_record,is_relay,source_url,raw)
    SELECT lpad($1::text || ':reviewed:' || number::text,64,'0'),'milesplit',
           $1::text || ':reviewed:' || number::text,'100 Meters','12.34','outdoor','recorded',
           false,false,'https://www.milesplit.com/meets/count-guard/results',
           jsonb_build_object('raw',jsonb_build_object('fixture',$1::text))
    FROM generate_series(1,$3::integer) number
    RETURNING *
), links AS (
    INSERT INTO reviewed_result_links(
        source_result_key,target_athlete_source_id,reviewed_by,researcher,reviewed_at,
        authorization_quote,manifest_sha256,evidence,result_snapshot,target_snapshot)
    SELECT r.source_result_key,p.id,'codex','gpt-6-luna',TIMESTAMPTZ '2026-11-21T00:00:00Z',
           'Authorized synthetic count-guard regression fixture',repeat('a',64),
           jsonb_build_array(jsonb_build_object('fixture',$1::text)),
           jsonb_build_object(
               'result_id',r.id,'source_result_key',r.source_result_key,'source_result_id',r.source_result_id,
               'source',r.source,'athlete_source_id',r.athlete_source_id,'athlete_id',r.athlete_id,
               'represented_school_id',r.represented_school_id,'event',r.event,'raw_mark',r.raw_mark,
               'sport',r.sport,'meet_id',r.meet_id,'meet_name',r.meet_name,'meet_date',r.meet_date,
               'wind',r.wind,'timing',r.timing,'specification',r.specification,'course',r.course,
               'status',r.status,'is_personal_record',r.is_personal_record,'is_relay',r.is_relay,
               'season',r.season,'grade',r.grade,'gender',r.gender,'source_url',r.source_url,
               'raw',r.raw,'normalized_numeric',r.normalized_numeric,'normalized_unit',r.normalized_unit),
           jsonb_build_object(
               'athlete_source_id',p.id,'source',p.source,'source_id',p.source_id,'profile_url',p.profile_url,
               'athlete_id',p.athlete_id,'profile_name',p.name,'profile_school_id',p.school_id,
               'profile_gender',p.gender,'profile_grade',p.grade,'profile_graduation_year',p.graduation_year,
               'athlete_name',a.name,'athlete_gender',a.gender,'athlete_graduation_year',a.graduation_year,
               'athlete_school_id',a.school_id,'athlete_state',a.state,
               'school_source',NULL,'school_source_id',NULL,'school_name',NULL,'school_state',NULL)
    FROM reviewed r CROSS JOIN profile p CROSS JOIN athlete a
    RETURNING source_result_key
)
SELECT a.id AS athlete_id,p.id AS profile_id,
       (SELECT count(*) FROM owned) AS owned_count,(SELECT count(*) FROM links) AS reviewed_count
FROM athlete a CROSS JOIN profile p
