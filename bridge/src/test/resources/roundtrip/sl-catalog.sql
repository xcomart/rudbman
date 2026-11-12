-- master
select type, name, tbl_name, sql from sqlite_master where name not like 'sqlite_autoindex%'
--;;
-- autoindex
select tbl_name, count(*) from sqlite_master where name like 'sqlite_autoindex%' group by tbl_name
--;;
-- sequence
select name, seq from sqlite_sequence
--;;
-- xinfo
select m.name, x.cid, x.name, x.type, x."notnull", x.dflt_value, x.pk, x.hidden from sqlite_master m, pragma_table_xinfo(m.name) x where m.type = 'table' and m.name not like 'sqlite_%'
--;;
-- fk
select m.name, f.id, f.seq, f."table", f."from", f."to", f.on_update, f.on_delete from sqlite_master m, pragma_foreign_key_list(m.name) f where m.type = 'table'
--;;
-- typeof
select id, typeof(i), typeof(r), typeof(t), typeof(b), typeof(n), typeof(d) from types
