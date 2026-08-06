# 진행 현황과 인수인계

다른 세션(또는 다른 사람)이 이어받기 위한 문서다. 설계와 계약은 전부
[architecture.md](architecture.md)에 있고, 여기는 **어디까지 왔고 무엇이
남았는지, 그리고 이 저장소에서 일하는 방식**만 담는다. 마일스톤 하나가 끝날
때마다 이 문서를 갱신한다.

최종 갱신: 2026-08-06 (M6 완료 직후).

## 어디까지 왔나

| 마일스톤 | 상태 | 들어간 것 |
|---|---|---|
| M0 | 완료 | 워크스페이스 셸, gpui 0.2.2 벤더링(logman 패치 6종, 바이트 동일 유지), rudbman-ui 위젯 16종, 테마/에디터 테마/설정/i18n 8개 언어, 고유 아이콘 |
| M1 | 완료 | 브리지 JAR(단일 JNI 진입점 `Bridge.call`, 오류 봉투, Gson 병합), JVM 부트스트랩(-Xrs, 전용 스레드, DestroyJavaVM 금지), 세션 워커, 드라이버 관리자(메이븐 다운로드·클래스 자동 검출), 접속 다이얼로그, SSH 터널(russh, PTY 없는 배스천, 루프백 바인드), OS 키체인 비밀 관리, URL/속성 마스킹 |
| M2 | 완료 | 탐색기 트리(멀티 루트, 레벨 스킵 규칙), DESCRIBE 전 종류, 테이블 상세 4탭(열·키·참조·DDL), DDL 역생성(native/metadata 이원) |
| M3 | 완료 | rudbman-sql(증분 렉서, 방언 7종, 문장 분리), rudbman-editor(ropey, IME — gpui 예제의 조합 캐럿 버그 수정), rudbman-grid(양축 가상화, 100만 행), 쿼리 파이프라인(실행/취소/세대 가드/NearEnd 페이징/다중 결과/쓰기 확인), RDB1 코덱 양단 |
| M4 | 완료 | 브리지 job 프레임(0x40~42) + ExtractJob(DDL은 CREATE 전부→FK ALTER 전부, insert/csv/template), jdbgen 템플릿 엔진 승계(자산 호환 카나리아 테스트), Rust Job API, 추출 다이얼로그, SQL 파일 열기(Ctrl+O) → 기존 실행 파이프라인 |
| M5 | 완료 | rudbman-erd 크레이트(모델·격자/자체 Sugiyama 배치·직교 라우팅·SVG 내보내기·캔버스 위젯 — 순수/gpui 모듈 분리), `PaneItem::Erd`+`ErdPane`(로딩·툴바), FK 로더(테이블당 imported_keys, 결정적 정렬), 배치 저장 `erd/<uuid>.json`(제스처당 1회), 탐색기 `OpenErd`(Ctrl/Cmd+E, 스코프 단위) |
| M6 | 완료 | 브리지 `TransferJob`(두 세션 락 핸들 오름차순, 취소 2슬롯, `uses(Session)` 훅, upsert 방언 3계열 + PK/OTHER 동기 거절, on_error abort/skip/log — savepoint로 배치 격리, errors 상한 100)·`BackupJob`(스코프 열거, INSERT만 FK 위상 정렬, gzip)·`meta/Upsert`, 진행률 `rows_skipped`, Rust `TransferSpec`/`BackupSpec`+`start_transfer`/`start_backup`, 전송·백업 다이얼로그(추출 폴링 패턴, 타깃 연결 셀렉트) |
| 중간 UI 작업 | 완료 | 미니탭(팬마다 탭 목록, 중복 열기=이동), **연결별 작업 영역**(상단 탭 전환이 하단 전체를 전환 — 사용자 확정 설계), 포커스 회수 규율(아래 "함정"), 에디터 폰트 설정 배선 |

- 저장소: <https://github.com/xcomart/rudbman> (public, MIT).
- 브랜치 흐름은 logman과 동일: **dev에서 작업, main은 PR 머지 커밋만**. CI는
  3플랫폼 매트릭스이고 브리지(Java) 스위트를 Rust보다 먼저 돌린다.
- 테스트 규모(2026-08-06): Rust 약 740 + Java 141. 전부 실제 JVM/H2를 부팅하는
  통합 테스트를 포함한다.

## 다음 작업

1. **M7 — 쿼리 빌더, 마무리**: ERD 캔버스(팬·줌·드래그·히트 판정)를 공유한다
   (§7.7). PNG 내보내기는 여전히 SVG만(§12.5). 전송·백업의 PostgreSQL/MySQL
   실물 검증(컨테이너 선택 테스트)과 Oracle/SQL Server/DB2 MERGE 철자 확인도
   남아 있다(브리지 README의 알려진 공백).

### 마일스톤에 안 걸린 미결

- LOB_READ(0x25) 브리지 미구현 → LOB 뷰어 없음. 재읽기 전략 후보는 §12.7.
- PL/SQL 블록·MySQL DELIMITER 문장 분리 미지원(rudbman-sql 테스트로 한계를
  고정해 둠).
- 파일 읽기 실패가 로그로만 남음 — 셸에 일시 메시지 스트립이 없다. 공용 알림
  UI가 생기면 연결할 것.
- 미니탭 다듬기: 활성 탭 스크롤 인뷰, 탭 컨텍스트 메뉴, 드래그 재배열.
- 연결 A의 쓰기 확인 모달이 연결 B로 전환한 뒤에도 떠 있음(응답 가능하고
  올바르나 시각적으로 어색 — 낮은 우선순위).
- 스페인어/독일어 UI 격식 수준 통일(사용자 답변 대기), 기타 용어 플래그.
- release.yml은 첫 릴리스 때. jlink 이미지에는 `jdk.charsets` 모듈이 필요하다
  (템플릿 엔진의 EUC-KR 패딩 폭, §6 패키징 노트).

## 이 저장소에서 일하는 방식

- **문서 먼저.** 와이어 계약·설계 결정은 코드보다 architecture.md를 먼저
  고친다. 브리지(Java)와 Rust 코덱/바인딩은 서로 다른 작업자가 문서만 보고
  작성해도 맞물리는 것이 목표였고, 실제로 그렇게 만들어졌다.
- **커밋 메시지는 영어 산문**으로 "왜"를 담는다. Co-authored-by 금지.
- **머지는 CI 3플랫폼 green 확인 후 머지 커밋**(`gh pr merge --merge`).
- vendor/gpui는 logman의 벤더 트리와 **바이트 동일**을 유지한다(패치 교환을
  diff로 하기 위함). 수정 금지.
- 검증 명령: `cargo test --workspace`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo fmt --check`, `cd bridge && ./gradlew build`.
  브리지 JAR 재생성은 `cd bridge && ./gradlew jar`.
- JVM/H2 통합 테스트는 H2 드라이버 JAR을 Gradle 캐시에서 스스로 찾는다.
  못 찾으면 `RUDBMAN_TEST_H2_JAR`로 지정(CI가 이 방식 — Windows에서는
  `cygpath -w`로 네이티브 철자로 변환해야 한다는 것이 첫 CI 런의 교훈).
  자동 탐색은 `HOME`(또는 `GRADLE_USER_HOME`)으로 Gradle 홈을 찾으므로
  **Windows PowerShell에서는 실패한다**(`HOME`이 없다) — Git Bash에서는 그냥
  되고, PowerShell에서는 `RUDBMAN_TEST_H2_JAR`를 지정하라.

## 개발 환경 함정 (이 머신 한정 포함)

- **이 X 디스플레이에서 gpui 앱은 키보드·마우스 입력을 못 받는 경우가 있다**
  (포인터 장치가 없어져 XInput2 초기화 실패). GUI 확인은 ① 환경변수로 게이트한
  임시 훅(`RUDBMAN_DEV_AUTOCONNECT` — 인메모리 H2 자동 연결, 필요한 화면을
  코드로 열기) + ② 스크린샷으로 한다. 훅은 **커밋 전 반드시 되돌리고 diff로
  확인**한다. 실행 시 `XDG_CONFIG_HOME`을 격리해 실제 설정을 오염시키지
  않는다.
- 스크린샷: `xwd`는 이 디스플레이에서 X_QueryColors로 죽는다. XGetImage를
  쓰는 작은 C 도구(스크래치패드의 xshot.c 참고 — libX11 링크)로 캡처한다.
- **`pkill -f` 금지** — 자기 셸까지 죽인다(명령줄 매칭). 반드시 PID로 kill.
- 개발 머신에서 사용자가 logman 등 다른 빌드를 병행할 수 있다 — cargo 락
  경합으로 빌드가 느려질 수 있음.

## 작업 분담 (Advisor/Worker)

메인 세션은 설계·브리프 작성·검증(diff와 테스트를 직접 실행)·커밋만 하고,
구현은 Opus 서브에이전트에게 위임한다. 브리프에는 파일 경로·컨벤션·함정·완료
기준을 담아 재탐색을 없앤다. 완료 보고는 믿지 않고 diff·테스트로 검증 후
승인한다. 독립 조각은 파일 범위를 겹치지 않게 잘라 병렬로 위임한다.
