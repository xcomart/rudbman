-- column
select object_name(c.object_id), c.name, c.column_id, type_name(c.user_type_id), type_name(c.system_type_id), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, c.is_computed, c.collation_name, c.is_rowguidcol from sys.columns c join sys.objects o on o.object_id = c.object_id where o.schema_id = schema_id('bk') and o.type in ('U', 'V')
--;;
-- type
select name, type_name(system_type_id), max_length, is_nullable from sys.types where schema_id = schema_id('bk')
--;;
-- identity
select object_name(object_id), name, cast(seed_value as varchar(40)), cast(increment_value as varchar(40)), cast(last_value as varchar(40)) from sys.identity_columns where object_schema_name(object_id) = 'bk'
--;;
-- default
select object_name(parent_object_id), col_name(parent_object_id, parent_column_id), name, definition from sys.default_constraints where schema_id = schema_id('bk')
--;;
-- check
select object_name(parent_object_id), name, definition, is_disabled, is_not_trusted from sys.check_constraints where schema_id = schema_id('bk')
--;;
-- key
select object_name(parent_object_id), name, type, unique_index_id from sys.key_constraints where schema_id = schema_id('bk')
--;;
-- fk
select object_name(parent_object_id), name, object_name(referenced_object_id), delete_referential_action_desc, update_referential_action_desc, is_disabled, is_not_trusted from sys.foreign_keys where schema_id = schema_id('bk')
--;;
-- index
select object_name(i.object_id), i.name, i.type_desc, i.is_unique, i.is_primary_key, i.is_unique_constraint, i.has_filter, i.filter_definition, i.fill_factor, (select string_agg(col_name(ic.object_id, ic.column_id) + case when ic.is_descending_key = 1 then ' desc' else '' end + case when ic.is_included_column = 1 then ' incl' else '' end, ',') within group (order by ic.is_included_column, ic.key_ordinal, ic.column_id) from sys.index_columns ic where ic.object_id = i.object_id and ic.index_id = i.index_id) from sys.indexes i join sys.objects o on o.object_id = i.object_id where o.schema_id = schema_id('bk') and i.type > 0
--;;
-- computed
select object_name(object_id), name, definition, is_persisted from sys.computed_columns where object_schema_name(object_id) = 'bk'
--;;
-- module
select o.name, o.type, rtrim(m.definition, char(13) + char(10)), m.uses_ansi_nulls, m.uses_quoted_identifier, m.is_schema_bound from sys.sql_modules m join sys.objects o on o.object_id = m.object_id where o.schema_id = schema_id('bk')
--;;
-- trigger
select name, object_name(parent_id), is_disabled, is_instead_of_trigger from sys.triggers where object_schema_name(parent_id) = 'bk'
--;;
-- view
select name, with_check_option from sys.views where schema_id = schema_id('bk')
--;;
-- sequence
select name, type_name(system_type_id), cast(start_value as varchar(40)), cast(increment as varchar(40)), cast(minimum_value as varchar(40)), cast(maximum_value as varchar(40)), is_cycling, is_cached, cache_size from sys.sequences where schema_id = schema_id('bk')
--;;
-- extprop
select object_name(major_id), minor_id, name, cast(value as nvarchar(4000)) from sys.extended_properties where class = 1 and object_schema_name(major_id) = 'bk'
--;;
-- table
select name, lock_escalation_desc, is_memory_optimized, durability_desc from sys.tables where schema_id = schema_id('bk')
--;;
-- view-result
select * from bk.v_fn
--;;
-- tvf-result
select * from bk.f_children(1)
--;;
-- probe-seq
select name, 'select next value for bk.' + quotename(name) from sys.sequences where schema_id = schema_id('bk')
--;;
-- probe-ident
select object_name(object_id), 'select ident_current(''bk.' + object_name(object_id) + ''')' from sys.identity_columns where object_schema_name(object_id) = 'bk'
