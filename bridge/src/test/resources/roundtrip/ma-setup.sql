create table bk.parent (id int auto_increment primary key, code varchar(20) not null comment 'business code', note text, amount decimal(12,3) not null default 0, created timestamp(3) default current_timestamp(3), flag tinyint(1) default 1, status enum('a','b','c') default 'a', tags set('x','y','z'), latin varchar(10) character set latin1 collate latin1_swedish_ci, unique key parent_code_uq (code), constraint ck_amount check (amount >= 0)) comment = 'parents ''q'' 한글'
--;;
create table bk.child (id int auto_increment primary key, parent_id int not null, qty int, twice int generated always as (qty * 2) stored, half int as (qty div 2) virtual, check (qty <> 13), key ix_child_parent (parent_id), key ix_child_parent_qty (parent_id, qty desc), constraint fk_child_parent foreign key (parent_id) references bk.parent (id) on delete cascade) engine = InnoDB auto_increment = 100
--;;
create table bk.`Order` (`select` int, `Group Id` int, `Value` varchar(50), primary key (`select`, `Group Id`), unique key `Order_value_uq` (`Value`(10)))
--;;
create table bk.a (id int primary key, b_id int)
--;;
create table bk.b (id int primary key, a_id int, constraint fk_b_a foreign key (a_id) references bk.a (id))
--;;
alter table bk.a add constraint fk_a_b foreign key (b_id) references bk.b (id)
--;;
create table bk.tree (id int primary key, parent_id int, name varchar(20), foreign key (parent_id) references bk.tree (id))
--;;
create table bk.types (id bigint unsigned auto_increment primary key, ti tinyint, si smallint, mi mediumint, bi bigint, d decimal(38,10), f float, db double, c char(3), vc varchar(50), t text, mt mediumtext, dt date, tm time(3), dtm datetime(6), ts timestamp(6) null, yr year, bn binary(4), vb varbinary(10), bl blob, js json, bt bit(8), en enum('x','y'), u uuid, ip inet6, fulltext key ft_t (t))
--;;
create table bk.empty_t (id int primary key, x text) engine = MyISAM
--;;
create table bk.audit (id bigint auto_increment primary key, what varchar(100), at datetime default current_timestamp)
--;;
insert into bk.parent (code, note, amount, status, tags, latin) values ('p1', 'semi;colon', 1.5, 'b', 'x,z', 'abc'), ('p2', 'it''s 한글 😀\nline2\ttab \\ back', 0, 'c', '', 'åäö'), ('p3', null, 99999.999, null, null, null), ('p4', '', 1, 'a', 'y', ''), ('p5', 'x', 2, 'a', 'y', 'x')
--;;
delete from bk.parent where code in ('p4', 'p5')
--;;
insert into bk.child (parent_id, qty) values (1, 10), (2, 20), (1, 300), (3, null)
--;;
insert into bk.`Order` values (1, 1, 'one'), (1, 2, 'two'), (2, 1, null)
--;;
insert into bk.a values (1, null), (2, null)
--;;
insert into bk.b values (1, 1), (2, 2)
--;;
update bk.a set b_id = id
--;;
set @old_mode = @@session.sql_mode
--;;
set session sql_mode = concat(@@session.sql_mode, ',NO_AUTO_VALUE_ON_ZERO')
--;;
insert into bk.audit (id, what) values (0, 'zero \\ row')
--;;
set session sql_mode = @old_mode
--;;
insert into bk.tree values (1, null, 'root'), (2, 1, 'kid'), (3, 2, 'grandkid')
--;;
insert into bk.types (ti, si, mi, bi, d, f, db, c, vc, t, mt, dt, tm, dtm, ts, yr, bn, vb, bl, js, bt, en, u, ip) values (-128, -32768, 8388607, 9223372036854775807, -1234567890123456789012345678.0123456789, 1.5, 3.141592653589793, 'ab', 'O''Brien', 'multi\nline', 'm', '2024-02-29', '23:59:59.123', '2024-02-29 23:59:59.123456', '2024-02-29 23:59:59.123456', 2024, 0x00ff1000, 0x00, 0x0001ff, '{"k": [1, "a;b"], "n": null}', b'1010', 'y', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '::ffff:192.168.0.1')
--;;
insert into bk.types (id, ti) values (10, 1)
--;;
insert into bk.types (ti) values (2), (3)
--;;
insert into bk.types (ti, t) values (null, '')
--;;
create sequence bk.seq_used start with 100 increment by 5 minvalue 50 maxvalue 100000 nocache cycle
--;;
select nextval(bk.seq_used), nextval(bk.seq_used)
--;;
create sequence bk.seq_unused start with 7 nocache
--;;
create table bk.uses_seq (id int primary key default (next value for bk.seq_unused), x int)
--;;
create function bk.child_count() returns int reads sql data comment 'counts' return (select count(*) from bk.child)
--;;
create function bk.fmt_int(x int) returns varchar(20) deterministic return concat('int:', x)
--;;
create procedure bk.p_bump(in delta int) modifies sql data begin update bk.child set qty = qty + delta; select 'a;b' as s; end
--;;
create trigger bk.trg_touch before insert on bk.child for each row set new.qty = coalesce(new.qty, 0) + 1
--;;
create trigger bk.trg_audit after update on bk.parent for each row begin insert into bk.audit (what) values (concat('upd;', new.id)); end
--;;
create trigger bk.trg_audit2 after update on bk.parent for each row follows trg_audit insert into bk.audit (what) values ('second')
--;;
create view bk.z_base as select id, code from bk.parent
--;;
create view bk.a_on_z as select id, upper(code) as code from bk.z_base where id > 0
--;;
create sql security invoker view bk.v_fn as select bk.child_count() as n, bk.fmt_int(1) as f1
--;;
create view bk.v_chk as select id, parent_id, qty from bk.child where qty > 0 with local check option
