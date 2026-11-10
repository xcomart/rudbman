-- exec
set session information_schema_stats_expiry = 0
--;;
-- schema
select default_character_set_name, default_collation_name from information_schema.schemata where schema_name = 'bk'
--;;
-- table
select table_name, table_type, engine, row_format, table_collation, auto_increment, create_options, table_comment from information_schema.tables where table_schema = 'bk'
--;;
-- column
select table_name, column_name, ordinal_position, column_default, is_nullable, column_type, character_set_name, collation_name, extra, generation_expression, column_comment, srs_id from information_schema.columns where table_schema = 'bk'
--;;
-- constraint
select table_name, constraint_name, constraint_type, enforced from information_schema.table_constraints where table_schema = 'bk'
--;;
-- check
select constraint_name, check_clause from information_schema.check_constraints where constraint_schema = 'bk'
--;;
-- fk
select constraint_name, table_name, referenced_table_name, update_rule, delete_rule, match_option from information_schema.referential_constraints where constraint_schema = 'bk'
--;;
-- key
select constraint_name, table_name, column_name, ordinal_position, referenced_table_name, referenced_column_name from information_schema.key_column_usage where table_schema = 'bk'
--;;
-- index
select table_name, index_name, non_unique, seq_in_index, column_name, collation, sub_part, index_type, is_visible, expression from information_schema.statistics where table_schema = 'bk'
--;;
-- view
select table_name, view_definition, check_option, is_updatable, security_type, character_set_client, collation_connection from information_schema.views where table_schema = 'bk'
--;;
-- routine
select routine_name, routine_type, data_type, dtd_identifier, routine_definition, is_deterministic, sql_data_access, security_type, sql_mode, routine_comment, character_set_client, collation_connection, database_collation from information_schema.routines where routine_schema = 'bk'
--;;
-- param
select specific_name, ordinal_position, parameter_mode, parameter_name, dtd_identifier from information_schema.parameters where specific_schema = 'bk'
--;;
-- trigger
select trigger_name, event_manipulation, event_object_table, action_order, action_statement, action_timing, sql_mode, character_set_client, collation_connection, database_collation from information_schema.triggers where trigger_schema = 'bk'
--;;
-- view-result
select * from bk.v_fn
