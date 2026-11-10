-- column
select tabname, colname, colno, typeschema, typename, length, scale, nulls, default, identity, generated, text, codepage, remarks, hidden from syscat.columns where tabschema = 'BK'
--;;
-- identity
select tabname, colname, increment, minvalue, maxvalue, cycle, cache, order from syscat.colidentattributes where tabschema = 'BK'
--;;
-- constraint
select tabname, case when constname like 'SQL%' then 'SYS' else constname end, type, enforced, trusted from syscat.tabconst where tabschema = 'BK'
--;;
-- check
select tabname, case when constname like 'SQL%' then 'SYS' else constname end, type, text from syscat.checks where tabschema = 'BK'
--;;
-- ref
select tabname, case when constname like 'SQL%' then 'SYS' else constname end, reftabname, fk_colnames, pk_colnames, deleterule, updaterule from syscat.references where tabschema = 'BK'
--;;
-- index
select i.tabname, case when i.indname like 'SQL%' then 'SYS' else i.indname end, i.uniquerule, i.indextype, i.system_required, (select listagg(coalesce(u.text, u.colname) || ' ' || u.colorder, ',') within group (order by u.colseq) from syscat.indexcoluse u where u.indschema = i.indschema and u.indname = i.indname) from syscat.indexes i where i.tabschema = 'BK'
--;;
-- sequence
select seqname, datatypeid, increment, minvalue, maxvalue, cycle, cache, order from syscat.sequences where seqschema = 'BK' and seqtype = 'S'
--;;
-- routine
select routinename, routinetype, parm_count, language, deterministic, sql_data_access, valid, text from syscat.routines where routineschema = 'BK'
--;;
-- trigger
select trigname, tabname, trigtime, trigevent, granularity, valid, text from syscat.triggers where trigschema = 'BK'
--;;
-- view
select viewname, viewcheck, readonly, valid, text from syscat.views where viewschema = 'BK'
--;;
-- table
select tabname, type, remarks from syscat.tables where tabschema = 'BK'
--;;
-- view-result
select * from bk.v_fn
--;;
-- probe-seq
select seqname, 'values next value for bk.' || seqname from syscat.sequences where seqschema = 'BK' and seqtype = 'S'
--;;
-- probe-rollback-identity
select 'PARENT', 'select id from final table (insert into bk.parent (code, amount) values (''probe'', 0))' from sysibm.sysdummy1 union all select 'CHILD', 'select id from final table (insert into bk.child (parent_id, qty) values (1, 1))' from sysibm.sysdummy1 union all select 'TYPES', 'select id from final table (insert into bk.types (si) values (0))' from sysibm.sysdummy1 union all select 'AUDIT_LOG', 'select id from final table (insert into bk.audit_log (what) values (''probe''))' from sysibm.sysdummy1
--;;
-- ts12
select id, varchar(ts12), varchar(df) from bk.types
