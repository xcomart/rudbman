-- column
select table_name, column_name, column_id, data_type, data_length, data_precision, data_scale, nullable, data_default_vc, char_used, char_length, virtual_column, identity_column, default_on_null from user_tab_cols where hidden_column = 'NO' and table_name not like 'BIN$%'
--;;
-- identity
select table_name, column_name, generation_type, regexp_replace(identity_options, 'START WITH: [0-9]+, ', '') from user_tab_identity_cols
--;;
-- constraint
select c.table_name, case when c.generated = 'GENERATED NAME' then 'SYS' else c.constraint_name end, c.constraint_type, c.search_condition_vc, (select r.table_name from user_constraints r where r.constraint_name = c.r_constraint_name), c.delete_rule, c.deferrable, c.deferred, c.status, c.validated, (select listagg(cc.column_name, ',') within group (order by cc.position) from user_cons_columns cc where cc.constraint_name = c.constraint_name) from user_constraints c where c.table_name not like 'BIN$%'
--;;
-- index
select i.table_name, case when i.generated = 'Y' then 'SYS' else i.index_name end, i.index_type, i.uniqueness, (select listagg(ic.column_name || ' ' || ic.descend, ',') within group (order by ic.column_position) from user_ind_columns ic where ic.index_name = i.index_name), i.status from user_indexes i where i.index_type <> 'LOB'
--;;
-- index-expr
select table_name, index_name, column_position, column_expression from user_ind_expressions
--;;
-- sequence
select sequence_name, min_value, max_value, increment_by, cycle_flag, order_flag, cache_size from user_sequences where sequence_name not like 'ISEQ$$%'
--;;
-- source
select name, type, line, text from user_source
--;;
-- view
select view_name, text_vc, read_only from user_views
--;;
-- mview
select mview_name, refresh_mode, refresh_method, build_mode, rewrite_enabled, query from user_mviews
--;;
-- trigger
select trigger_name, trigger_type, triggering_event, table_name, status, trigger_body from user_triggers
--;;
-- tab-comment
select table_name, table_type, comments from user_tab_comments where comments is not null
--;;
-- col-comment
select table_name, column_name, comments from user_col_comments where comments is not null
--;;
-- object
select object_name, object_type, status from user_objects where object_name not like 'SYS_%' and object_name not like 'ISEQ$$%' and object_name not like 'BIN$%'
--;;
-- view-result
select * from v_fn
--;;
-- mv-result
select * from mv_counts
--;;
-- probe-seq
select sequence_name, 'select ' || sequence_name || '.nextval from dual' from user_sequences where sequence_name not like 'ISEQ$$%'
--;;
-- probe-identity
select table_name || '.' || column_name, 'select ' || sequence_name || '.nextval from dual' from user_tab_identity_cols
