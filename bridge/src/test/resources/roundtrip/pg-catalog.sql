-- constraint
select conrelid::regclass::text, contypid::regtype::text, conname, contype, pg_get_constraintdef(oid), condeferrable, condeferred from pg_constraint where connamespace = 'bk'::regnamespace
--;;
-- index
select indexrelid::regclass::text, pg_get_indexdef(indexrelid) from pg_index i join pg_class c on c.oid = i.indrelid where c.relnamespace = 'bk'::regnamespace
--;;
-- attr
select a.attrelid::regclass::text, a.attname, a.attnum, format_type(a.atttypid, a.atttypmod), a.attnotnull, a.attidentity, a.attgenerated, pg_get_expr(d.adbin, d.adrelid), a.attcollation from pg_attribute a join pg_class c on c.oid = a.attrelid left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum where c.relnamespace = 'bk'::regnamespace and a.attnum > 0 and not a.attisdropped and c.relkind in ('r', 'v', 'm', 'p')
--;;
-- rel
select c.relname, c.relkind, c.relpersistence, c.reloptions, c.relhastriggers from pg_class c where c.relnamespace = 'bk'::regnamespace
--;;
-- view
select viewname, definition from pg_views where schemaname = 'bk'
--;;
-- matview
select matviewname, definition, ispopulated, hasindexes from pg_matviews where schemaname = 'bk'
--;;
-- function
select p.oid::regprocedure::text, p.prokind, pg_get_functiondef(p.oid) from pg_proc p where p.pronamespace = 'bk'::regnamespace and not exists (select 1 from pg_depend d where d.objid = p.oid and d.deptype = 'e')
--;;
-- ext-functions
select count(*) from pg_proc p where p.pronamespace = 'bk'::regnamespace
--;;
-- trigger
select tgrelid::regclass::text, tgname, pg_get_triggerdef(oid), tgenabled from pg_trigger where not tgisinternal and tgrelid in (select oid from pg_class where relnamespace = 'bk'::regnamespace)
--;;
-- sequence
select sequencename, data_type::text, start_value, min_value, max_value, increment_by, cycle, cache_size, last_value from pg_sequences where schemaname = 'bk'
--;;
-- seq-owner
select s.relname, d.deptype, d.refobjid::regclass::text, d.refobjsubid from pg_depend d join pg_class s on s.oid = d.objid and s.relkind = 'S' where s.relnamespace = 'bk'::regnamespace and d.refclassid = 'pg_class'::regclass
--;;
-- type
select t.typname, t.typtype, format_type(t.typbasetype, t.typtypmod), t.typnotnull, t.typdefault, (select string_agg(enumlabel, ',' order by enumsortorder) from pg_enum where enumtypid = t.oid) from pg_type t where t.typnamespace = 'bk'::regnamespace and t.typtype in ('e', 'd')
--;;
-- comment
select c.relname, d.objsubid, d.description from pg_description d join pg_class c on c.oid = d.objoid where c.relnamespace = 'bk'::regnamespace
--;;
-- extension
select extname, extversion, extnamespace::regnamespace::text from pg_extension
--;;
-- view-result
select * from bk.v_fn
--;;
-- view-result2
select * from bk.a_on_z
--;;
-- mv-result
select * from bk.mv_counts
--;;
-- probe-nextval
select sequencename, 'select nextval(' || quote_literal('bk.' || quote_ident(sequencename)) || ')' from pg_sequences where schemaname = 'bk'
