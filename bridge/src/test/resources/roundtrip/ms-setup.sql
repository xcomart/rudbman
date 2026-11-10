create type bk.code_t from nvarchar(20) not null
--;;
create sequence bk.seq_used as bigint start with 100 increment by 5 minvalue 50 maxvalue 100000 cycle cache 10
--;;
select next value for bk.seq_used
--;;
select next value for bk.seq_used
--;;
create sequence bk.seq_unused start with 7
--;;
create table bk.parent (id int identity(1,1) constraint pk_parent primary key, code bk.code_t constraint parent_code_uq unique, note nvarchar(max), amount decimal(12,3) not null constraint df_amount default 0 constraint ck_amount check (amount >= 0), created datetime2(3) constraint df_created default sysdatetime(), flag bit default 1, h uniqueidentifier default newid())
--;;
exec sp_addextendedproperty 'MS_Description', N'parents ''q'' 한글', 'SCHEMA', 'bk', 'TABLE', 'parent'
--;;
exec sp_addextendedproperty 'MS_Description', N'business code', 'SCHEMA', 'bk', 'TABLE', 'parent', 'COLUMN', 'code'
--;;
create table bk.child (id int identity(10,5) primary key, parent_id int not null references bk.parent(id) on delete cascade, qty int check (qty <> 13), twice as (qty * 2), thrice as (qty * 3) persisted)
--;;
create index ix_child_parent on bk.child (parent_id)
--;;
create index ix_child_parent_qty on bk.child (parent_id, qty desc) include (thrice)
--;;
create index ix_child_filtered on bk.child (qty) where qty > 100
--;;
create table bk.[Order] ([select] int not null, [Group Id] int not null, [Value] nvarchar(50), constraint pk_order primary key nonclustered ([select], [Group Id]))
--;;
create unique clustered index ix_order_value on bk.[Order] ([Value])
--;;
create table bk.a (id int primary key, b_id int)
--;;
create table bk.b (id int primary key, a_id int constraint fk_b_a references bk.a(id))
--;;
alter table bk.a add constraint fk_a_b foreign key (b_id) references bk.b(id)
--;;
create table bk.tree (id int primary key, parent_id int references bk.tree(id), name nvarchar(20))
--;;
create table bk.types (id bigint identity primary key, ti tinyint, si smallint, bi bigint, d decimal(38,10), r real, f float, c char(3), vc varchar(50), nc nchar(3), nvc nvarchar(50), vmax varchar(max), dt date, tm time(3), dtm datetime, dt2 datetime2(7), dto datetimeoffset(3), sdt smalldatetime, bn binary(4), vb varbinary(max), b bit, m money, u uniqueidentifier, x xml)
--;;
create table bk.empty_t (id int primary key, x nvarchar(10) collate Latin1_General_CS_AS)
--;;
create table bk.audit (id bigint identity primary key, what nvarchar(100), at datetime2 default sysdatetime())
--;;
insert into bk.parent (code, note, amount) values ('p1', N'semi;colon', 1.5), ('p2', N'it''s 한글 😀' + char(10) + 'line2' + char(9) + 'tab \ back', 0), ('p3', null, 99999.999), ('p4', '', 1), ('p5', 'x', 2)
--;;
delete from bk.parent where code in ('p4', 'p5')
--;;
insert into bk.child (parent_id, qty) values (1, 10), (2, 20), (1, 300), (3, null)
--;;
insert into bk.[Order] values (1, 1, 'one'), (1, 2, 'two'), (2, 1, null)
--;;
insert into bk.a values (1, null), (2, null)
--;;
insert into bk.b values (1, 1), (2, 2)
--;;
update bk.a set b_id = id
--;;
insert into bk.tree values (1, null, 'root'), (2, 1, 'kid'), (3, 2, 'grandkid')
--;;
insert into bk.types (ti, si, bi, d, r, f, c, vc, nc, nvc, vmax, dt, tm, dtm, dt2, dto, sdt, bn, vb, b, m, u, x) values (255, -32768, 9223372036854775807, -1234567890123456789012345678.0123456789, 1.5, 3.141592653589793, 'ab', 'O''Brien', N'가나', N'한글 😀', replicate(cast('z' as varchar(max)), 9000), '2024-02-29', '23:59:59.123', '2024-02-29 23:59:59.997', '2024-02-29 23:59:59.1234567', '2024-02-29 23:59:59.123 +09:00', '2024-02-29 23:59', 0x00ff1000, 0x0001ff, 1, 12.3456, 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11', '<a b="1">x &amp; y</a>')
--;;
set identity_insert bk.types on
--;;
insert into bk.types (id, ti) values (10, 1)
--;;
set identity_insert bk.types off
--;;
insert into bk.types (ti) values (2), (3)
--;;
insert into bk.types (ti, vc) values (null, '')
--;;
create function bk.child_count() returns int as begin return (select count(*) from bk.child); end
--;;
create function bk.f_children(@p int) returns table as return (select id, qty from bk.child where parent_id = @p)
--;;
create procedure bk.p_bump @delta int as begin set nocount on; update bk.child set qty = qty + @delta; select 'a;b' as s; end
--;;
create trigger bk.trg_touch on bk.child after insert as begin set nocount on; update c set qty = coalesce(c.qty, 0) + 1 from bk.child c join inserted i on i.id = c.id; end
--;;
create trigger bk.trg_audit on bk.parent after update as begin set nocount on; insert into bk.audit (what) values ('upd;'); end
--;;
create view bk.z_base as select id, code from bk.parent
--;;
create view bk.a_on_z as select id, upper(code) as code from bk.z_base where id > 0
--;;
create view bk.v_fn as select bk.child_count() as n
--;;
create view bk.v_chk as select id, parent_id, qty from bk.child where qty > 0 with check option
--;;
create view bk.v_bound with schemabinding as select id, qty from bk.child
--;;
create unique clustered index ix_v_bound on bk.v_bound (id)
