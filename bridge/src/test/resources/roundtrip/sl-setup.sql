create table parent (id integer primary key autoincrement, code text not null constraint parent_code_uq unique, note text, amount numeric(12,3) not null default 0 constraint ck_amount check (amount >= 0), created text default current_timestamp, flag integer default 1, r blob)
--;;
create table child (id integer primary key, parent_id integer not null references parent(id) on delete cascade, qty integer check (qty <> 13), twice integer generated always as (qty * 2) virtual, thrice integer generated always as (qty * 3) stored)
--;;
create index ix_child_parent on child (parent_id)
--;;
create index ix_child_parent_qty on child (parent_id, qty desc)
--;;
create index ix_child_partial on child (qty) where qty > 100
--;;
create index ix_parent_lower on parent (lower(code))
--;;
create table "Order" ("select" integer, "Group Id" integer, "Value" text, primary key ("select", "Group Id")) without rowid
--;;
create table a (id integer primary key, b_id integer references b(id))
--;;
create table b (id integer primary key, a_id integer references a(id))
--;;
create table tree (id integer primary key, parent_id integer references tree(id), name text)
--;;
create table types (id integer primary key, i integer, r real, t text, b blob, n numeric, d date, dt datetime)
--;;
create table empty_t (id integer primary key, x text)
--;;
create table audit_log (id integer primary key autoincrement, what text)
--;;
insert into parent (code, note, amount, r) values ('p1', 'semi;colon', 1.5, x'00FF10'), ('p2', 'it''s 한글 😀' || char(10) || 'line2' || char(9) || 'tab \ back', 0, null), ('p3', null, 99999.999, null), ('p4', '', 1, null), ('p5', 'x', 2, null)
--;;
delete from parent where code in ('p4', 'p5')
--;;
insert into child (parent_id, qty) values (1, 10), (2, 20), (1, 300), (3, null)
--;;
insert into "Order" values (1, 1, 'one'), (1, 2, 'two'), (2, 1, null)
--;;
insert into a values (1, 1), (2, 2)
--;;
insert into b values (1, 1), (2, 2)
--;;
insert into tree values (1, null, 'root'), (2, 1, 'kid'), (3, 2, 'grandkid')
--;;
insert into types (i, r, t, b, n, d, dt) values (9223372036854775807, 3.141592653589793, 'multi' || char(10) || 'line', x'0001FF', 1.25, '2024-02-29', '2024-02-29 23:59:59.123')
--;;
insert into types (id, i) values (10, 1)
--;;
insert into types (i, t) values (2, ''), (3, null)
--;;
insert into types (i, t) values ('text in int', 42)
--;;
create trigger trg_touch after insert on child for each row begin update child set qty = coalesce(new.qty, 0) + 1 where id = new.id; end
--;;
create trigger trg_audit after update on parent begin insert into audit_log (what) values ('upd;'); end
--;;
create view z_base as select id, code from parent
--;;
create view a_on_z as select id, upper(code) as code from z_base where id > 0
