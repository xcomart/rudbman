-- column
select table_name, column_name, ordinal_position, data_type, character_maximum_length, numeric_precision, numeric_scale, datetime_precision, is_nullable, column_default, is_identity, identity_generation, identity_start, identity_increment, identity_maximum, identity_minimum, identity_cycle, is_generated, generation_expression, domain_name, remarks, collation_name from information_schema.columns where table_schema = 'BK'
--;;
-- constraint
select table_name, constraint_name, constraint_type from information_schema.table_constraints where table_schema = 'BK'
--;;
-- check
select constraint_name, check_clause from information_schema.check_constraints where constraint_schema = 'BK'
--;;
-- ref
select constraint_name, unique_constraint_name, update_rule, delete_rule from information_schema.referential_constraints where constraint_schema = 'BK'
--;;
-- key
select constraint_name, table_name, column_name, ordinal_position from information_schema.key_column_usage where table_schema = 'BK'
--;;
-- index
select i.table_name, i.index_name, i.index_type_name, (select listagg(c.column_name || ' ' || c.ordering_specification, ',') within group (order by c.ordinal_position) from information_schema.index_columns c where c.index_schema = i.index_schema and c.index_name = i.index_name) from information_schema.indexes i where i.table_schema = 'BK'
--;;
-- view
select table_name, view_definition from information_schema.views where table_schema = 'BK'
--;;
-- sequence
select sequence_name, data_type, start_value, minimum_value, maximum_value, increment, cycle_option, cache from information_schema.sequences where sequence_schema = 'BK'
--;;
-- routine
select routine_name, routine_type, external_name from information_schema.routines where routine_schema = 'BK'
--;;
-- trigger
select trigger_name, event_manipulation, event_object_table, action_timing, java_class from information_schema.triggers where trigger_schema = 'BK'
--;;
-- domain
select domain_name, data_type, domain_default from information_schema.domains where domain_schema = 'BK'
--;;
-- domain-check
select d.domain_name, c.check_clause from information_schema.domain_constraints d join information_schema.check_constraints c on c.constraint_schema = d.constraint_schema and c.constraint_name = d.constraint_name where d.domain_schema = 'BK'
--;;
-- table
select table_name, table_type, remarks from information_schema.tables where table_schema = 'BK'
--;;
-- probe-seq
select sequence_name, 'values next value for bk.' || sequence_name from information_schema.sequences where sequence_schema = 'BK' and sequence_name not like 'SYSTEM_SEQUENCE%'
--;;
-- probe-identity
select table_name, 'select identity_base from information_schema.columns where table_schema = ''BK'' and table_name = ''' || table_name || ''' and is_identity = ''YES''' from information_schema.columns where table_schema = 'BK' and is_identity = 'YES'
