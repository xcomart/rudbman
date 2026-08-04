# rudbman 아키텍처

JDBC 드라이버를 JNI로 감싸고 Rust + gpui로 GUI를 올린 데이터베이스 작업 도구.
데이터베이스 탐색, 쿼리 빌드/실행, ERD 생성, 스크립트 추출/실행, 백업,
DB→DB 데이터 전송을 하나의 창에서 수행한다.

이 문서는 코드가 쓰이기 전에 경계를 확정하기 위한 것이다. 여기서 정하지 않은
것은 구현자가 정해도 되고, 여기서 정한 것은 이 문서를 고치지 않고는 바꾸지
않는다.

---

## 1. 결정 요약

| # | 결정 | 이유 |
|---|---|---|
| D1 | logman의 gpui 위젯 킷을 **복사**해 `rudbman-ui`로 독립 진화 | 초기 속도 우선. 공유 크레이트 추출은 양쪽이 안정된 뒤로 미룬다 |
| D2 | logman의 **패치된 gpui를 벤더링**해 그대로 사용 | 한글 IME 무한 루프, X11 재진입 패닉, KWin 블러 패치가 rudbman에도 그대로 필요하다 |
| D3 | JNI 경계는 **굵은 입자 브리지 JAR** 하나. 진입점은 정적 메서드 **1개** | 셀 단위 JNI 왕복은 10만 행 × 20열에서 수천만 회가 된다. 쓸 수 없다 |
| D4 | 백업·DB→DB 전송의 **데이터 플레인은 JVM 안에서 완결** | 수 GB를 JNI로 퍼 나르지 않는다. Rust는 명령 발행과 진행률 폴링만 |
| D5 | 커넥션마다 **전용 Rust 워커 스레드**가 JVM에 상주 attach | JDBC 커넥션은 스레드 안전하지 않고 gpui UI 스레드는 블로킹 금지 |
| D6 | 드라이버마다 자식 `URLClassLoader`로 **격리** | Oracle/MySQL/MSSQL 드라이버 간 클래스 충돌 방지. jdbgen이 이미 이 방식 |
| D7 | **jlink 번들 런타임**을 플랫폼 패키지에 동봉 | 사용자에게 JRE 설치를 요구하지 않는다 |
| D8 | 결과 배치는 **자체 컬럼너 바이너리 코덱**, 메타데이터는 JSON | Arrow Java는 40MB+ 의존성과 `--add-opens`를 요구한다. 그리드가 필요로 하는 것은 그보다 훨씬 적다 |
| D9 | **UI 테마와 에디터 테마를 분리**된 토큰 집합으로 | 창 크롬의 11색과 구문 강조의 20여 색은 다른 축이다 |
| D10 | **SSH 로컬 포트 포워딩을 M1 범위에 포함** | 운영 DB는 대개 배스천 뒤에 있다. 나중에 붙이면 접속 프로파일 스키마와 세션 수명 관리를 두 번 고치게 된다 |

---

## 2. 기존 자산 승계

### 2.1 logman (`~/Work/logman`) — UI 전부

복사 대상 (`crates/logman-app/src/` 기준):

| 원본 | 목적지 | 비고 |
|---|---|---|
| `ui/` 14개 파일 6.5k라인 | `crates/rudbman-ui/src/` | `logman_input` 액션 네임스페이스를 `rudbman_input`으로 개명 |
| `theme_store.rs` | `crates/rudbman-ui/src/theme_store.rs` | 에디터 테마 디렉터리를 추가로 로드하도록 확장 |
| `theme_editor.rs` | `crates/rudbman-app/src/theme_editor.rs` | 에디터 테마 탭 추가 |
| `pane_tree.rs` | `crates/rudbman-app/src/pane_tree.rs` | 터미널 대신 에디터/그리드/ERD 패널을 담도록 제네릭화 |
| `caption.rs` | `crates/rudbman-app/src/caption.rs` | 그대로 |
| `i18n.rs` + `locales/*.yml` | 동일 | 키만 교체 |
| `icons.rs` | 동일 | DB 관련 아이콘 추가 |
| `about_dialog.rs` | 동일 | 문자열만 교체 |
| `logman-core/` 전부 | `crates/rudbman-core/` | `settings`/`profile`/`secrets`/`paths`. 프로파일 스키마만 교체 |
| `logman-ssh/` | `crates/rudbman-ssh/` | M1. 전송·인증·호스트키만 승계, 셸/SFTP 제거, 포워딩 추가 (§9) |
| `known_hosts.rs`, `verifier.rs` | `rudbman-core` / `rudbman-app` | 호스트 키 저장과 확인 다이얼로그 |
| `vendor/gpui/` | 동일 | **`LOGMAN PATCH` 주석을 그대로 둔다** — 두 벤더본을 `diff`로 동기화할 수 있게 바이트 동일하게 유지. 상류 릴리스 전엔 삭제 금지 |

가져오지 않는 것: `logman-pty`, `logman-term`, `terminal_view.rs`,
`file_panel.rs`, `connection.rs`(SSH 셸 전용), `files.rs`,
`logman-ssh`의 `sftp.rs`.

`ui/scheme_picker.rs`는 터미널 색 구성표 미리보기용이지만, 에디터 테마
미리보기로 형태가 거의 그대로 쓰인다. 복사 후 개조한다.

### 2.2 jdbgen (`~/Work/jdbgen`) — Java 계층

브리지 JAR의 출발점이다. 승계 대상:

- `types/db/DBMeta.java` (417줄) — 드라이버별 `URLClassLoader`, `DriverManager`를
  우회한 `Driver.connect` 직접 호출(드라이버 등록 전역 상태 회피), `ReentrantLock`
  기반 커넥션 직렬화, keep-alive 스케줄러, 스키마/테이블/컬럼 조회
- `types/db/SqlTypes.java`, `DBColumn`, `DBTable`, `DBSchema`, `DBMetaModel`
- `utils/ClassUtils.java` — JAR을 스캔해 `java.sql.Driver` 구현 클래스를 찾는다
  (드라이버 등록 UI에서 클래스명 자동 검출)
- `utils/MavenREST.java`, `ui/MavenExplorer.java` — 메이븐 중앙에서 드라이버 JAR
  다운로드. **로직은 Rust로 이식**한다(HTTP는 Rust가 하는 편이 낫다)
- `template/TemplateManager.java` — 스크립트 추출 템플릿 엔진. M4에서 판단:
  Java에 두고 브리지로 호출하거나, Rust로 이식하거나
- `resources/icons/*.png` 13종 — 드라이버 아이콘

**DBMeta에 없어서 새로 써야 하는 것**: 외래 키(`getImportedKeys`/`getExportedKeys`),
인덱스(`getIndexInfo`), 기본 키(`getPrimaryKeys`), 뷰/프로시저/시퀀스, DDL 역생성,
`ResultSetMetaData` 기반 쿼리 결과 스키마.

---

## 3. 저장소 레이아웃

```
rudbman/
├── Cargo.toml                  워크스페이스. [patch.crates-io] gpui → vendor/gpui
├── docs/architecture.md        이 문서
├── vendor/gpui/                logman 패치본
├── bridge/                     Gradle 프로젝트 → rudbman-bridge.jar
│   ├── build.gradle
│   └── src/main/java/comart/rudbman/bridge/
├── runtime/                    jlink 산출물(빌드 시 생성, .gitignore)
├── assets/                     아이콘
├── packaging/                  linux/macos/windows 패키징
└── crates/
    ├── rudbman-core/           설정·프로파일·시크릿·경로·known_hosts
    ├── rudbman-ui/             gpui 위젯 킷 + UI 테마 + 에디터 테마
    ├── rudbman-ssh/            SSH 로컬 포트 포워딩 (M1)
    ├── rudbman-jdbc/           JNI. JVM 부트스트랩·세션 워커·와이어 코덱
    ├── rudbman-sql/            SQL 렉서·방언·포매터·자동완성 인덱스
    ├── rudbman-editor/         멀티라인 코드 에디터 위젯
    ├── rudbman-grid/           가상화 결과 그리드 위젯
    ├── rudbman-erd/            ERD 모델·레이아웃·캔버스·SVG 내보내기
    └── rudbman-app/            바이너리
```

### 3.1 크레이트 의존 방향

```
rudbman-app
 ├─→ rudbman-erd ─┐
 ├─→ rudbman-grid ┼─→ rudbman-ui ─→ gpui
 ├─→ rudbman-editor ─→ rudbman-sql
 ├─→ rudbman-jdbc ─→ rudbman-core
 ├─→ rudbman-ssh  ─→ rudbman-core
 └─→ rudbman-core
```

`rudbman-ssh`와 `rudbman-jdbc`는 **서로를 모른다.** 터널은 로컬 포트를 열어줄
뿐이고, JDBC 세션은 그 포트를 향한 평범한 접속일 뿐이다. 둘을 묶는 것은
`rudbman-app`의 세션 오케스트레이션이다 (§9.3).

역방향 의존은 없다. `rudbman-jdbc`는 gpui를 **모른다** — 순수 동기 API를 노출하고,
UI 스레드와의 결합은 `rudbman-app`이 담당한다. 이 경계 덕분에 JNI 계층을 gpui 없이
단위 테스트할 수 있다.

`rudbman-ui`는 데이터베이스 개념을 모른다. logman의 `ui/` 모듈이 SSH를 몰랐던 것과
같은 규율이다.

---

## 4. JNI 계층 (`rudbman-jdbc`)

### 4.1 JVM 생명주기

- 프로세스당 JVM 인스턴스 **하나**. `OnceLock<JavaVM>`에 보관.
- `jni = { version = "0.22", features = ["invocation"] }`.
- **`DestroyJavaVM`을 호출하지 않는다.** 신뢰할 수 없고, 프로세스 종료가 정리한다.
- JVM 생성은 **전용 백그라운드 스레드**에서 한다. macOS에서 gpui가 메인 스레드를
  점유하고 있고, JVM 생성 스레드는 살아 있어야 한다.
- 런타임 위치 결정 순서:
  1. 실행 파일 옆의 번들 런타임 (`<exe_dir>/../runtime`, macOS는 `Contents/runtime`)
  2. 환경 변수 `RUDBMAN_JAVA_HOME`
  3. `JAVA_HOME`
  결정된 경로를 `JAVA_HOME`으로 설정한 **뒤** `JavaVM::new()`를 부른다.

JVM 옵션(고정):

```
-Djava.class.path=<bridge.jar>
-Djava.awt.headless=true
-Xrs                      # JVM이 SIGINT/SIGTERM 핸들러를 가로채지 못하게 한다.
                          # 이게 없으면 Ctrl-C와 창 닫기가 JVM에 삼켜진다.
-Xss2m                    # 일부 드라이버(특히 Oracle)의 깊은 스택
-XX:+UseSerialGC          # 데스크톱 도구의 힙은 작다. 병렬 GC 스레드가 아깝다
-Duser.language / -Duser.country   # 앱 로케일과 일치시켜 드라이버 오류 메시지를 맞춘다
```

`-Xmx`는 설정에서 조정 가능하게 노출한다(기본 1g). 큰 결과 집합 fetch가 JVM 힙을
먼저 때린다.

### 4.2 스레드 모델

```
gpui UI 스레드
    │  Command + oneshot 응답 채널
    ▼
Session 워커 스레드  ──  AttachCurrentThreadAsDaemon 후 상주
    │  Bridge.call(op, handle, arg, req)
    ▼
Java: 세션 객체 (Connection 1개 + 자식 ClassLoader)
```

- 커넥션 1개당 워커 스레드 1개. 워커는 명령 큐를 직렬 처리한다. JDBC 커넥션의
  스레드 비안전성이 여기서 구조적으로 해소된다.
- 워커는 attach 상태를 유지한다. 명령마다 attach/detach 하면 JVM이 매번 스레드
  구조체를 만든다.
- **취소만은 예외**: `CANCEL`은 워커가 블로킹된 동안 다른 스레드에서 들어와야
  한다. 취소 전용 경로는 호출마다 attach/detach 한다(드물게 일어나는 일이다).
- UI는 `cx.background_spawn`으로 워커 응답을 await 한다. UI 스레드는 절대
  블로킹하지 않는다.
- 워커 스레드 패닉은 세션을 죽이되 프로세스를 죽이지 않는다. JNI 호출은
  `catch_unwind` 경계 안에서 이뤄진다.

### 4.3 브리지 진입점

Java 쪽 진입점은 **정적 메서드 하나**다:

```java
package comart.rudbman.bridge;

public final class Bridge {
    /**
     * @param op     연산 코드 (§4.4)
     * @param handle 세션/커서/작업 핸들. 연산에 따라 의미가 다르다. 0 = 없음
     * @param arg    핫 패스용 정수 인자 (FETCH의 최대 행 수 등). JSON 파싱 회피용
     * @param req    요청 본문. JSON UTF-8 또는 null
     * @return       응답 봉투 (§4.5). 절대 null이 아니고 예외를 던지지 않는다
     */
    public static byte[] call(int op, long handle, long arg, byte[] req);
}
```

Rust는 `jmethodID` 하나만 캐시한다. Java 쪽은 모든 본문을 try/catch로 감싸
예외를 응답 봉투의 오류 태그로 바꾼다 — **JNI 예외 검사가 정상 경로에서 사라진다.**
`ExceptionCheck`는 OOM 같은 치명적 상황에서만 걸린다.

새 기능은 연산 코드 추가로 붙는다. JNI 시그니처는 영원히 그대로다.

### 4.4 연산 코드

| 코드 | 이름 | handle | arg | req | resp |
|---|---|---|---|---|---|
| `0x01` | `OPEN_SESSION` | — | — | JSON 접속 명세 | JSON `{session}` |
| `0x02` | `CLOSE_SESSION` | session | — | — | — |
| `0x03` | `PING` | session | — | — | JSON `{ok, elapsed_ms}` |
| `0x04` | `SESSION_INFO` | session | — | — | JSON DB 제품·버전·기능 플래그 |
| `0x10` | `DESCRIBE` | session | — | JSON `{kind, …}` | JSON |
| `0x20` | `EXECUTE` | session | — | JSON `{sql, params, fetch_size, max_rows, timeout_s}` | JSON `{cursor, columns[], update_count, may_have_more}` |
| `0x21` | `FETCH` | cursor | 최대 행 수 | — | **바이너리 배치** (§4.6) |
| `0x22` | `MORE_RESULTS` | cursor | — | — | JSON, `EXECUTE`와 동형 |
| `0x23` | `CLOSE_CURSOR` | cursor | — | — | — |
| `0x24` | `CANCEL` | session | — | — | — |
| `0x25` | `LOB_READ` | cursor | — | JSON `{lob_id, offset, len}` | 바이너리 |
| `0x30` | `SET_AUTOCOMMIT` | session | 0/1 | — | — |
| `0x31` | `COMMIT` | session | — | — | — |
| `0x32` | `ROLLBACK` | session | — | — | — |
| `0x40` | `JOB_START` | session | — | JSON 작업 명세 (§6) | JSON `{job}` |
| `0x41` | `JOB_POLL` | job | — | — | JSON 진행률 |
| `0x42` | `JOB_CANCEL` | job | — | — | — |
| `0x50` | `PROBE_DRIVER` | — | — | JSON `{jars[]}` | JSON `{classes[]}` |

`DESCRIBE`의 `kind`: `catalogs`, `schemas`, `tables`, `columns`, `primary_keys`,
`imported_keys`, `exported_keys`, `indexes`, `procedures`, `functions`,
`sequences`, `ddl`, `type_info`.

`DESCRIBE`가 요청 JSON으로 분기하는 이유: 메타데이터 종류는 앞으로 계속 늘어나고,
그때마다 연산 코드를 늘리면 Rust와 Java의 표가 어긋난다. 메타데이터는 호출 빈도가
낮으니 JSON 파싱 비용은 무시할 수 있다.

아직 구현되지 않은 연산과 `kind`는 `kind: "protocol"` 오류로 **"not implemented"**를
답한다. **"unknown"과 구별되어야 한다** — 전자는 기다리면 되는 것이고 후자는 양쪽
표가 어긋났다는 뜻이다.

#### `may_have_more`는 힌트다

JDBC에는 비파괴적 예견(lookahead)이 없다. 현재 결과를 소비하지 않고 다음 결과가
있는지 알 방법이 **없다.** 그래서 이 필드는 "`MORE_RESULTS`가 무언가를 돌려줄 수도
있다"는 보수적 힌트일 뿐이다.

**Rust는 단일 값을 믿지 말고 `false`가 나올 때까지 반복하라.** 소진은 다음 세 가지가
동시에 성립하는 응답이다: `may_have_more: false`, `update_count: -1`, `columns` 없음.

#### `EXECUTE`의 `params`

바인딩 파라미터는 둘 중 한 형태다:

```json
"params": [42, "text", true, null,
           {"type": "decimal",   "value": "123456789012.12345678"},
           {"type": "timestamp", "value": "2026-08-04T09:30:00"},
           {"type": "bytes",     "value": "<base64>"}]
```

맨 JSON 스칼라는 정수·문자열·불리언·null까지만 쓴다. `decimal`, `date`, `time`,
`timestamp`, `bytes`는 **반드시 타입 형태로 보내라.** 이유는 §4.6이 반대 방향에
대해 금지하는 것과 같다 — `DECIMAL(20,8)`을 JSON 숫자로 보내면 반올림되어 도착하고,
그것은 되돌릴 수 없다.

### 4.5 응답 봉투

```
u8  tag       0 = OK, 1 = ERROR
    payload   OK이면 연산별 본문(JSON 또는 바이너리), ERROR이면 JSON
```

오류 JSON:

```json
{
  "kind": "sql | driver | io | protocol | interrupted | internal",
  "sql_state": "42S02",
  "vendor_code": 942,
  "message": "ORA-00942: table or view does not exist",
  "causes": ["…"],
  "stack": "…"
}
```

`sql_state`와 `vendor_code`가 있어야 UI가 "테이블 없음"과 "권한 없음"을 구별해
다르게 안내할 수 있다. `stack`은 디버그 로그에만 쓰고 사용자에게 보이지 않는다.

**`sql_state`는 전체 코드가 아니라 앞 두 자리(class)로 분기하라.** 표준을 지키는
드라이버끼리도 하위 두 자리가 갈린다 — 테이블 없음은 H2가 `42S04`, 다른 드라이버는
`42S02`다. 클래스 `42`(구문 오류 또는 접근 규칙 위반)까지가 믿을 수 있는 범위다.

### 4.6 결과 배치 바이너리 코덱 (`RDB1`)

전부 리틀 엔디언.

```
Batch  := Header Column*
Header := "RDB1"(4B) | u32 col_count | u32 row_count | u8 flags
          flags bit0 = 이 배치가 마지막
Column := u8 kind | u32 payload_len | payload
```

`payload`는 항상 **유효성 비트맵**으로 시작한다: `ceil(row_count/8)` 바이트,
비트가 서면 non-null. 그 뒤 종류별 값이 온다.

| kind | 이름 | 값 배치 |
|---|---|---|
| 0 | `NULLS` | 없음. 전 행이 NULL |
| 1 | `I64` | `row_count × i64` |
| 2 | `F64` | `row_count × f64` |
| 3 | `BOOL` | 팩된 비트 |
| 4 | `STR` | `u32 offsets[row_count+1]` + UTF-8 바이트 |
| 5 | `BIN` | `STR`과 동일 배치, 원시 바이트 |
| 6 | `LOB` | `row_count × (u64 lob_id, u64 size)`. 본문은 `LOB_READ`로 |

**`DECIMAL`, `DATE`, `TIME`, `TIMESTAMP`, `UUID`, `INTERVAL`, 배열, 기타 벤더 타입은
전부 `STR`로 정규 텍스트 표현을 실어 보낸다.** 이유:

- 그리드가 하는 일은 결국 텍스트 표시다
- `BigDecimal`의 정밀도와 스케일을 f64로 뭉개면 되돌릴 수 없다
- 시간대 지옥을 Rust로 옮겨오지 않는다. 드라이버가 준 텍스트가 진실이다
- 정렬은 `ORDER BY`로 서버가 한다. 클라이언트 수치 정렬이 필요 없다

논리 타입(`java.sql.Types` 값, 타입명, 정밀도, 스케일, nullable, 자동 증가 여부)은
`EXECUTE` 응답 JSON의 `columns[]`에 실린다. 코덱의 `kind`는 **물리 인코딩일 뿐**이고,
우측 정렬·NULL 표시·복사 포맷 같은 표현 결정은 논리 타입이 한다.

LOB은 배치에 인라인하지 않는다. 100MB BLOB이 든 행을 스크롤했다고 100MB를
JNI로 넘길 수는 없다. 셀을 열 때 `LOB_READ`로 청크 단위로 가져온다. **주소는
`lob_id` 하나뿐이다** — 커서 위치와 무관한 불투명 식별자여야 한다. `{row, col}`로
지정하면 결과 집합이 이미 지나간 행을 가리키게 된다.

#### 코덱 규범 — 인코더와 디코더가 반드시 일치해야 하는 것

명세가 열어 둔 지점들이다. 어긋나면 조용히 잘못된 데이터를 그린다.

- **비트맵 비트 순서는 LSB 우선.** 행 `i` → 바이트 `i >> 3`, 비트 `i & 7`. 팩된
  `BOOL` 값도 같다.
- **유효성 비트맵은 언제나 존재한다.** `NULLS`(kind 0)도 전부 0인 비트맵을 싣고,
  생략하는 것은 값 영역뿐이다.
- **같은 열의 kind가 배치마다 달라질 수 있다.** 어떤 열이든 그 배치에서 전부 NULL
  이면 `NULLS`로 축약된다. **디커더는 커서당 한 번이 아니라 배치마다 kind를 다시
  읽어야 한다.** `EXECUTE` 응답의 `columns[].kind`는 힌트일 뿐이다.
- **NULL 행도 고정 폭 값 영역에서 자리를 차지한다**(0으로 채움). 앞선 non-null
  개수를 세는 rank 계산이 필요 없다.
- **`STR`/`BIN`에서 NULL과 빈 문자열은 둘 다 길이 0 슬라이스다.** 구별하는 것은
  비트맵뿐이다.
- **`row_count == 0`이면 모든 열이 `NULLS`다.** `STR`이 `offsets[1]`을 요구하는
  경계가 이렇게 사라진다.
- **마지막 배치 플래그(bit0)는 드라이버가 행을 다 소진했을 때만 선다.** 배치가
  요청한 최대 행 수를 정확히 채우면 `flags = 0`이고, 다음 `FETCH`가 0행 + bit0을
  돌려준다.
- **`payload_len`은 비트맵과 값 영역을 합한 길이다.**
- **LOB `size`의 단위**는 이진 LOB이면 옥텟, 문자 LOB이면 문자다.
  `0xFFFFFFFFFFFFFFFF`는 드라이버가 크기를 답하지 않았다는 뜻이다.

#### 타입 매핑에서 판단이 들어간 곳

- **`BIT`는 정밀도로 갈린다.** `≤1`이면 `BOOL`, 그보다 크면 `BIN` — MySQL의
  `BIT(n)`은 바이트 문자열을 돌려준다.
- **`LONGVARCHAR`/`LONGVARBINARY`는 `LOB`이 아니라 `STR`/`BIN`으로 인라인된다.**
  MySQL의 `LONGTEXT`가 여기 걸리는 알려진 날카로운 모서리다.
- **`REAL`은 `F64`로 실린다.** 32비트 float가 64비트로 넓혀지므로 Rust가 f32
  정밀도로 표시하지 않으면 `0.1`이 `0.10000000149011612`로 보인다.

### 4.7 드라이버 격리

드라이버 하나당 자식 `URLClassLoader` 하나(부모 = 브리지의 로더). `DriverManager`를
쓰지 않고 `Class.forName(cls, true, child)` → `Driver.connect(url, props)`를 직접
부른다. `DriverManager`는 전역 등록부라서 두 드라이버가 같은 URL 접두사를 주장하면
어느 쪽이 이길지 알 수 없다.

`Driver.connect`가 `null`을 돌려주면 JDBC 명세상 "이 URL을 이해하지 못한다"는
뜻이다. 예외가 아니므로 명시적으로 검사해 사용자에게 "드라이버가 이 URL을 받지
않습니다"로 보고한다 (jdbgen이 이미 이렇게 한다).

같은 드라이버를 쓰는 세션끼리는 로더를 공유한다. 세션마다 새 로더를 만들면
드라이버 정적 초기화가 반복되고 메모리가 샌다. 로더는 `jar 경로 집합`을 키로
캐시하고, 마지막 세션이 닫힐 때 함께 닫는다.

---

## 5. Java 브리지 JAR

```
comart.rudbman.bridge/
├── Bridge.java          유일한 JNI 진입점. op 디스패치 + 예외 → 오류 봉투
├── Registry.java        핸들 ↔ 객체 표 (AtomicLong 발번, ConcurrentHashMap)
├── Session.java         Connection + ClassLoader + keep-alive  (DBMeta 계승)
├── Loaders.java         URLClassLoader 캐시
├── Cursor.java          Statement + ResultSet + 배치 인코더
├── codec/BatchWriter    §4.6 인코더
├── meta/Describe.java   DatabaseMetaData 조회 → JSON
├── meta/Ddl.java        DDL 역생성 (방언별)
├── job/BackupJob.java   §6
├── job/TransferJob.java §6
└── json/                최소 JSON 직렬화기 (Gson 의존 회피 여부는 미결, §12)
```

의존성은 최소로 유지한다. 브리지 JAR이 무거워질수록 jlink 이미지와 시동 시간이
같이 무거워진다. 현재 후보는 JSON 직렬화기 하나뿐이다.

`Session`은 jdbgen `DBMeta`의 커넥션 락을 **유지한다**. Rust 워커가 이미 명령을
직렬화하지만, keep-alive 타이머가 여전히 동시에 돈다.

---

## 6. 데이터 플레인 — 백업과 DB→DB 전송 (D4)

가장 중요한 설계 결정이다. **행 데이터가 JNI 경계를 넘지 않는다.**

```
JOB_START { kind: "transfer", source_cursor_sql, target_session, target_table,
            mode: "insert|upsert|truncate_insert", batch_size, commit_every,
            column_map[], on_error: "abort|skip|log" }
   ↓
Java: source ResultSet → target PreparedStatement.addBatch → executeBatch
      전 과정이 JVM 안에서 완결. 별도 작업 스레드에서 실행
   ↓
JOB_POLL → { state, rows_done, rows_total, bytes, phase, errors[], eta_s }
```

Rust는 `JOB_POLL`을 주기적으로(200ms 정도) 호출해 진행률 바를 그린다. 취소는
`JOB_CANCEL`이 작업 스레드의 인터럽트 플래그를 세우고 `Statement.cancel()`을 부른다.

백업도 같은 틀이다. `kind: "backup"`은 결과를 타겟 커넥션 대신 파일로 쓴다
(SQL INSERT 스크립트 / CSV / 압축). 파일 I/O도 Java가 한다 — 데이터가 있는 쪽에서.

`rows_total`은 대개 모른다. 사전 `COUNT(*)`는 선택 사항으로 두고(체크박스),
기본은 무한 진행률 + 처리 행 수 표시.

---

## 7. UI 계층

### 7.1 창 구조

logman의 셀프 드로우 타이틀바와 `pane_tree`를 그대로 승계한다.

```
┌ 타이틀바 (탭 = 접속 세션) ────────────────────────┬ 창 버튼 ┐
├───────────┬────────────────────────────────────────────────┤
│ 탐색기     │  pane_tree (분할 가능)                          │
│ 트리       │  ┌──────────────────────────────┐              │
│           │  │ SQL 에디터                      │              │
│ 서버       │  ├──────────────────────────────┤              │
│ └ 스키마    │  │ 결과 그리드 / 메시지 / 실행계획   │              │
│   ├ 테이블  │  └──────────────────────────────┘              │
│   ├ 뷰     │  또는 테이블 상세 / ERD 캔버스 / 쿼리 빌더        │
│   └ 프로시저 │                                                │
├───────────┴────────────────────────────────────────────────┤
│ 상태 표시줄 (접속·트랜잭션 상태·행 수·경과 시간)                  │
└─────────────────────────────────────────────────────────────┘
```

`pane_tree.rs`는 현재 터미널 뷰에 특화돼 있다. 패널 내용을 열거형
(`Editor | Grid | TableDetail | Erd | QueryBuilder`)으로 일반화하는 것이 이식
작업의 핵심이다.

### 7.2 UI 테마

logman의 11색 토큰(`background`, `surface`, `surface_hover`, `surface_active`,
`border`, `text`, `text_muted`, `accent`, `danger`, `success`, `overlay`)을
그대로 쓴다. 파일 포맷·레지스트리·편집기·사용자 디렉터리 로딩까지 전부 승계.

그리드용 토큰 몇 개를 추가한다: `grid_header`, `grid_row_alt`, `grid_selection`,
`grid_null`(NULL 셀의 흐린 표시), `grid_pk`(기본 키 열 강조). 기본값은 기존
토큰에서 파생해 손으로 쓴 테마 파일이 이 값들을 몰라도 읽히게 한다 — logman의
`icon` 슬롯이 쓰는 방식 그대로.

### 7.3 에디터 테마 (D9)

UI 테마와 **다른 파일, 다른 디렉터리, 다른 토큰 집합**이다.
`~/.config/rudbman/editor-themes/<id>.json`.

```json
{
  "version": 1,
  "name": "Tokyo Night",
  "dark": true,
  "colors": {
    "background": "#1a1b26",  "foreground": "#a9b1d6",
    "cursor": "#c0caf5",      "selection": "#33467c",
    "line_highlight": "#1f2335",
    "gutter": "#3b4261",      "gutter_active": "#737aa2",
    "keyword": "#bb9af7",     "string": "#9ece6a",
    "number": "#ff9e64",      "comment": "#565f89",
    "function": "#7aa2f7",    "type": "#2ac3de",
    "operator": "#89ddff",    "identifier": "#c0caf5",
    "punctuation": "#a9b1d6", "bracket_match": "#f7768e",
    "error": "#f7768e",       "warning": "#e0af68"
  }
}
```

로딩은 `theme_store.rs`의 `load_dir` 제네릭을 그대로 재사용한다(형식만 다른
세 번째 디렉터리). 테마 편집기에 탭을 하나 더 붙인다.

UI 테마와 에디터 테마는 독립 선택이지만, 설정에 "UI 테마를 따라감" 옵션을 두어
밝은 UI에 어두운 에디터가 딸려오는 사고를 막는다.

### 7.4 SQL 에디터 (`rudbman-editor`)

logman의 `TextInput`은 단일 라인 전용이다(`\n`을 공백으로 치환). 신규 작성한다.

- 버퍼: `ropey`. 100MB 스크립트를 열어도 편집이 O(log n)
- 렌더: gpui `uniform_list` 기반 가상화. 화면에 보이는 줄만 shape
- 입력: `EntityInputHandler` 구현. **IME 조합은 logman의 `text_input.rs`가 푼
  문제를 그대로 따른다** — 바이트 오프셋과 UTF-16 오프셋의 변환, 조합 중 캐럿 처리
- 구문 강조: `rudbman-sql`의 렉서. 트리시터 도입은 하지 않는다(SQL 방언마다 문법이
  갈리고, 강조에 필요한 것은 토큰 수준이다)
- 기능: 줄 번호, 현재 줄 강조, 괄호 짝, 다중 커서, 문장 단위 실행(커서 위치의
  문장 감지), 접기, 찾기/바꾸기, 자동 들여쓰기, 주석 토글
- 자동완성: 접속 세션의 스키마 인덱스에서 테이블·열·별칭을 제안. 인덱스는 접속
  직후 백그라운드로 채우고 메모리에 둔다

### 7.5 결과 그리드 (`rudbman-grid`)

- `uniform_list` 가상화 + 가로 가상화(열이 수백 개인 테이블이 있다)
- 열 너비 조절/고정/숨김/순서 변경, 정렬은 서버 왕복(`ORDER BY` 재실행)
- 셀 선택·범위 선택·복사(TSV/CSV/INSERT문/JSON)
- NULL과 빈 문자열을 시각적으로 구별한다. 이걸 못 하는 도구가 너무 많다
- 셀 편집 → `UPDATE` 생성 (기본 키가 있는 단일 테이블 쿼리에 한해)
- LOB 셀은 크기만 표시하고 클릭 시 뷰어에서 청크 로드
- 무한 스크롤: 바닥에 닿으면 `FETCH` 추가 배치. 배치 크기 기본 500행

### 7.6 ERD (`rudbman-erd`)

- 모델: `DESCRIBE imported_keys/exported_keys` 결과의 그래프
- 레이아웃: 1단계는 격자 배치 + 수동 드래그 + 위치 저장. 2단계에서 자동 배치
  (Sugiyama 계층 배치가 FK 방향성에 맞는다)
- 렌더: gpui `canvas`. 엔티티 박스 + 직교 라우팅 관계선 + 카디널리티 표기
- 내보내기: SVG를 직접 생성한다(gpui 오프스크린 렌더는 경로가 험하다). PNG는
  SVG 경유 또는 보류
- 캔버스·드래그·줌·팬 코드는 쿼리 빌더(§7.7)와 공유한다

### 7.7 쿼리 빌더

ERD 캔버스 위에 테이블을 올리고 조인을 그리면 SQL이 나온다. 열 선택, WHERE 조건
행, GROUP BY, 정렬을 폼으로 편집하고 결과를 에디터로 보낸다. 역방향(SQL → 빌더)은
하지 않는다 — 파서 복잡도가 도구 전체보다 커진다.

---

## 8. 설정·프로파일·시크릿

logman-core의 구조를 승계하고 스키마만 바꾼다.

```
~/.config/rudbman/            (macOS: ~/Library/Application Support/rudbman)
├── settings.json             앱 설정
├── connections.json          접속 프로파일 (비밀번호 제외)
├── drivers.json              드라이버 정의
├── themes/*.json             UI 테마
├── editor-themes/*.json      에디터 테마
├── snippets/*.sql            사용자 SQL 스니펫
├── history.db                쿼리 실행 이력 (SQLite? 미결 §12)
└── drivers/                  다운로드된 드라이버 JAR
```

접속 프로파일:

```json
{
  "id": "uuid",  "name": "운영 Oracle",  "folder": "운영",
  "color": "#e06c75",
  "driver_id": "oracle-thin",
  "url": "jdbc:oracle:thin:@//host:1521/ORCLPDB",
  "username": "app",
  "props": { "oracle.jdbc.ReadTimeout": "30000" },
  "keep_alive": { "enabled": true, "interval_s": 300, "query": "select 1 from dual" },
  "read_only": false,
  "auto_commit": true,
  "confirm_writes": true
}
```

비밀번호는 **파일에 저장하지 않는다.** `keyring`(logman-core `secrets.rs`)에
`rudbman:<profile-id>`로 넣는다.

`read_only`와 `confirm_writes`는 운영 DB 사고 방지용이다. `read_only`면 세션을
`Connection.setReadOnly(true)`로 열고 DDL/DML 실행 전에 막는다.

드라이버 정의:

```json
{
  "id": "oracle-thin",  "name": "Oracle Thin",  "icon": "oracle",
  "class": "oracle.jdbc.OracleDriver",
  "jars": ["~/.config/rudbman/drivers/ojdbc11-23.4.0.24.05.jar"],
  "maven": "com.oracle.database.jdbc:ojdbc11:23.4.0.24.05",
  "url_template": "jdbc:oracle:thin:@//{host}:{port}/{service}",
  "default_port": 1521,
  "dialect": "oracle"
}
```

`dialect`가 `rudbman-sql`의 키워드 집합, 식별자 인용 규칙, DDL 생성기, 페이징
구문(`ROWNUM` vs `LIMIT` vs `FETCH FIRST`)을 고른다.

---

## 9. SSH 터널 (`rudbman-ssh`)

운영 데이터베이스는 대개 배스천 호스트 뒤에 있다. 접속 프로파일이 터널을
선택적으로 끼고, JDBC는 그 결과로 열린 로컬 포트에 붙는다.

### 9.1 logman-ssh에서 승계하는 것과 새로 쓰는 것

**승계**: `SshConfig`/`SshAuth`(비밀번호·키·에이전트, `Debug`에서 시크릿 자동 마스킹),
전용 스레드 + 자체 Tokio 런타임 구조(GUI 스레드에서 핸들을 안전하게 들 수 있게 하는
그 설계), `HostKeyVerifier` 트레이트와 `known_hosts` 저장, `SshEvent` 스트림,
russh 설정(ring 백엔드, keepalive, 연결 타임아웃).

**새로 씀**: logman-ssh는 `channel_open_session`으로 셸 채널과 SFTP만 연다.
포트 포워딩은 없다. 필요한 것:

- `channel_open_direct_tcpip(remote_host, remote_port, origin_host, origin_port)`
- 로컬 `TcpListener` 수용 루프. 수용마다 채널 하나를 열고 양방향 복사
- PTY를 요청하지 않는 접속 경로. 터널에 셸은 필요 없고, 셸 없는 계정으로도
  포워딩만 허용된 배스천이 흔하다
- 채널 다중화: 커넥션 풀이 동시에 여러 소켓을 열면 채널도 여러 개가 된다

`sftp.rs`는 가져오지 않는다.

### 9.2 프로파일 스키마

접속 프로파일(§8)에 선택적 `tunnel` 블록이 붙는다:

```json
"tunnel": {
  "enabled": true,
  "host": "bastion.example.com", "port": 22,
  "username": "ops",
  "auth": "agent | key | password",
  "key_path": "~/.ssh/id_ed25519",
  "remote_host": "db.internal", "remote_port": 5432,
  "local_port": 0
}
```

`local_port: 0`이면 OS가 빈 포트를 고른다 — 기본값으로 둔다. 고정 포트는 두 세션이
같은 포트를 요구하면 충돌한다. 실제 바인딩된 포트를 JDBC URL의 `{host}:{port}`
자리에 치환해 넣는다.

터널의 비밀번호와 키 패스프레이즈도 `keyring`에 넣는다
(`rudbman:<profile-id>:tunnel`).

### 9.3 수명 관리

터널과 JDBC 세션은 서로를 모르는 두 자원이고, `rudbman-app`이 순서를 책임진다.

```
접속 시작 → 터널 수립 → 바인딩 포트 확인 → URL 치환 → OPEN_SESSION
접속 해제 → CLOSE_SESSION → 터널 해제
```

- **터널이 먼저 서고 나중에 눕는다.** JDBC 세션이 살아 있는데 터널이 먼저 닫히면
  드라이버는 원인 불명의 소켓 오류만 본다
- 터널 하나를 여러 세션이 공유한다(같은 배스천·같은 대상). 참조 계수로 관리하고
  마지막 세션이 닫힐 때 눕힌다
- 터널이 도중에 끊기면 그 위의 모든 JDBC 세션에 `kind: "io"` 오류를 전파하고
  세션을 죽은 상태로 표시한다. 조용히 재연결하지 않는다 — 트랜잭션 중이었을
  수 있고, 사용자가 알아야 한다
- 호스트 키가 처음 보는 것이면 지문을 보여주고 확인을 받는다. logman의
  `verifier.rs` 다이얼로그를 그대로 쓴다

---

## 10. 빌드와 패키징

### 10.1 브리지 JAR

Gradle이 `bridge/`를 빌드해 `bridge/build/libs/rudbman-bridge.jar`를 낸다.

`cargo build`가 Gradle을 부르지 않는다. `rudbman-jdbc/build.rs`는 JAR의 존재만
확인하고, 없으면 `./gradlew :bridge:jar`를 실행하라는 명확한 오류를 낸다.
Rust를 고칠 때마다 JVM이 뜨는 것은 견딜 수 없다.

전체 빌드는 `just`/`xtask` 한 곳에서 조율한다: `gradlew :bridge:jar` → `jlink` →
`cargo build --release` → 패키징.

### 10.2 jlink 런타임

```
jlink --add-modules \
    java.base,java.sql,java.sql.rowset,java.naming,java.transaction.xa,\
    java.security.jgss,java.security.sasl,java.management,java.logging,\
    jdk.crypto.ec,jdk.crypto.cryptoki,jdk.unsupported,jdk.net \
    --strip-debug --no-header-files --no-man-pages --compress=zip-6 \
    --output runtime/
```

- `jdk.unsupported`는 **필수**다. 상당수 드라이버가 `sun.misc.Unsafe`를 쓴다
- `java.naming`은 JNDI/LDAP 인증
- `java.security.jgss`/`sasl`은 Kerberos 통합 인증
- `java.transaction.xa`는 XA 지원 드라이버가 로드 시 참조한다

빠진 모듈은 접속 시점의 `NoClassDefFoundError`로만 드러난다. 각 드라이버별
스모크 테스트로 모듈 목록을 검증한다.

산출 크기 목표: 플랫폼당 50MB 내외.

### 10.3 배포 형태

logman의 `packaging/`을 승계한다. Linux는 tar + `install.sh` + `.desktop`,
macOS는 `.app` 번들(`Contents/runtime/`), Windows는 폴더 + 런처.

CI는 3플랫폼 매트릭스. 각 잡이 JDK 셋업 → Gradle → jlink → cargo 순으로 돈다.

### 10.4 브랜치와 릴리스

logman의 흐름을 그대로 따른다.

- 작업은 `dev`에서 한다. `main`은 CI가 3플랫폼에서 통과한 뒤
  `gh pr merge --merge`로 **머지 커밋을 통해서만** 움직인다
- 릴리스는 `main`의 머지 커밋을 가리키는 주석 태그 `vX.Y.Z`를 밀어서 낸다.
  `.github/workflows/release.yml`이 태그에서 아티팩트와 GitHub 릴리스를 만든다
- 태그를 밀기 전에 `[workspace.package] version`과 `Cargo.lock`
  (`cargo update --workspace`)을 별도 `chore:` 커밋으로 올린다
- **릴리스 노트는 주석 태그의 본문**이다(`release.yml`이 `%(contents:body)`를
  읽는다). 제목은 `rudbman vX.Y.Z`, 본문은 한 줄 도입부 뒤에 **사용자에게 보이는
  변화 하나당 불릿 하나**. 문단 산문은 릴리스 페이지에서 벽이 된다. 구현이 아니라
  사용자 관점으로 쓰고 72자에서 접는다
- 이미 공개된 릴리스에 늦은 변경을 접어 넣을 때: `main`에 머지 → 원격 태그 삭제
  → 새 머지 커밋에 태그 재생성 → 푸시. 단, 릴리스 액션은 기존 릴리스의
  **아티팩트만 교체하고 본문은 남긴다**. 재릴리스 후에는
  `gh release edit vX.Y.Z --notes-file <file>`로 노트를 손으로 밀어야 하고,
  남은 초안이 있는지 확인해 지운다

`.github/workflows/ci.yml`과 `release.yml`은 logman에서 가져와 Gradle·jlink
단계를 추가한다.

---

## 11. 마일스톤

| | 범위 | 완료 기준 |
|---|---|---|
| **M0** | 워크스페이스, gpui 벤더링, `rudbman-ui`·`rudbman-core` 이식, 테마/설정/i18n | 빈 창이 뜨고 테마 전환과 설정 저장이 동작. `cargo test` 통과 |
| **M1** | 브리지 JAR 최소본, JVM 부트스트랩, 세션 워커, 드라이버 관리자, 접속 다이얼로그, **SSH 터널** | H2·PostgreSQL·MySQL 접속/해제/PING 왕복. 오류 봉투가 UI에 표시됨. 배스천 경유 PostgreSQL 접속 |
| **M2** | 탐색기 트리, `DESCRIBE` 전 종류, 테이블 상세(열·키·인덱스·FK·DDL) | 세 DB에서 스키마 탐색과 DDL 표시 |
| **M3** | `rudbman-sql`, `rudbman-editor`, `rudbman-grid`, 실행/취소/다중 결과 | 100만 행 결과를 끊김 없이 스크롤. 실행 중 취소 동작. IME 입력 정상 |
| **M4** | 스크립트 추출(DDL/DML), 스크립트 실행, 템플릿 엔진 | 테이블 → CREATE/INSERT 스크립트, 스크립트 파일 실행 + 오류 보고 |
| **M5** | ERD 모델·레이아웃·캔버스·SVG 내보내기 | FK 있는 스키마의 ERD 생성, 드래그 배치 저장, SVG 출력 |
| **M6** | 백업, DB→DB 전송 (§6) | 100만 행을 PostgreSQL → MySQL 전송, 진행률·취소·오류 보고 |
| **M7** | 비주얼 쿼리 빌더 | 3테이블 조인 쿼리를 GUI로 구성해 실행 |

M3이 전체 작업량의 40% 안팎이다. M0~M3이 "쓸 만한 도구"의 최소선이다.

### 테스트 전략

- `rudbman-jdbc`는 gpui에 의존하지 않으므로 순수 통합 테스트가 가능하다.
  **H2 인메모리를 기준 DB로 삼는다** — jdbgen이 이미 `sample_h2.db.mv.db`를 갖고 있다
- 코덱은 Java 인코더/Rust 디코더 왕복 속성 테스트
- 위젯은 logman처럼 `gpui/test-support`의 헤드리스 플랫폼으로
- PostgreSQL/MySQL/Oracle은 로컬 컨테이너 기반 선택적 테스트(CI 기본 제외)

---

## 12. 미결 사항

1. **브리지 JAR의 JSON 라이브러리** — Gson(230KB, 검증됨) vs 자체 최소 직렬화기.
   자체 구현은 의존성 0이지만 이스케이프 처리에서 버그가 나기 쉽다. M1에서 결정.
2. **쿼리 이력 저장소** — SQLite(rusqlite, 네이티브 의존성 추가) vs JSON Lines
   (단순, 검색 느림). 이력 규모 예상치가 나오면 결정.
3. **스크립트 추출 템플릿 엔진** — jdbgen의 Java 구현을 브리지에 두고 호출할지,
   Rust로 이식할지. Rust 이식이 깔끔하지만 기존 템플릿 자산과의 호환이 걸린다.
4. **ERD 자동 배치 알고리즘** — Sugiyama 자체 구현 vs 기존 크레이트. M5에서 조사.
5. **PNG 내보내기** — gpui 오프스크린 렌더 경로 확인 필요. SVG만으로 시작.
6. **SSH 에이전트 전달과 점프 호스트 다단** — 배스천이 둘 이상 겹치는 환경이
   있다. M1은 단일 홉만 지원하고, 필요가 확인되면 확장한다.
7. **LOB 재읽기 전략** — `lob_id`로 주소는 정해졌지만(§4.6), 대부분의 드라이버는
   **행이 바뀌는 순간 `Blob`/`Clob` 핸들을 무효화한다.** 그리드가 500행을 가져온
   뒤 사용자가 세 번째 행의 BLOB을 열면 그 핸들은 이미 죽어 있다. 후보:
   (a) fetch 시점에 임시 파일로 흘려보내기 — 정확하지만 열지도 않을 LOB에 디스크를
   쓴다, (b) 인라인 상한(4KB 정도)까지만 즉시 읽고 그 이상은 기본 키로 단일 행을
   재질의 — 키 없는 결과 집합에서는 거부해야 한다, (c) 현재 배치에 한해서만 허용.
   `LOB_READ`를 구현하는 M3에서 결정한다. 브리지는 이미 `lob_id → (행, 열, 크기,
   이진 여부)`를 기록하고 있어 어느 쪽도 뒤에 붙일 수 있다.

---

## 부록 A. 지켜야 할 함정

logman과 jdbgen이 이미 대가를 치르고 배운 것들이다.

- **gpui 벤더 패치를 지운다** → 한글 IME로 타이핑하면 CPU 100%로 굳는다. X11에서
  마지막 창을 닫으면 패닉한다. `vendor/gpui`의 `LOGMAN PATCH` 주석 6곳(taffy 1,
  x11 client 1, x11 window 5, windows 3)을 지우거나 개명하지 말 것 — logman의
  벤더본과 바이트 동일해야 `diff`로 상류 패치를 주고받을 수 있다
- **`DriverManager`를 쓴다** → 두 드라이버가 같은 URL 접두사를 주장하면 승자를
  알 수 없다. `Driver.connect`를 직접 부를 것
- **`Driver.connect`의 `null` 반환을 무시한다** → 명세상 "URL을 이해 못 함"이다.
  예외가 아니다
- **`-Xrs` 없이 JVM을 띄운다** → JVM이 SIGINT/SIGTERM을 가로채 창 닫기가 먹통이 된다
- **세션마다 새 `URLClassLoader`** → 드라이버 정적 초기화 반복, 메모리 누수
- **결과를 UI 스레드에서 fetch** → 창이 굳는다. 워커 스레드 경계를 넘지 말 것
- **`DECIMAL`을 `f64`로** → 되돌릴 수 없다. 텍스트로 실어 보낼 것
- **LOB을 배치에 인라인** → 100MB BLOB 한 행이 JNI를 통과한다
- **gpui 콜백 안에서 `RefCell`을 든 채 다른 콜백 호출** → X11 백엔드에서 재진입
  패닉. logman이 두 번 겪었다
