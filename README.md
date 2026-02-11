# whichport

로컬 머신에서 현재 `LISTEN` 중인 TCP 포트를 조회하고, 각 포트를 점유한 프로세스와 용도를 추정해 보여주는 Rust CLI입니다.

## 주요 기능

- 포트 지정 조회: `whichport <port...>`
- 전체 조회: `whichport --all`
- 자동화용 JSON 출력: `--json`
- 메타데이터 출력: `--verbose` (텍스트 출력에서만)
- Linux에서 `ss` 우선, 실패 시 `lsof` 폴백
- 동일 프로세스의 IPv4/IPv6 바인딩을 하나로 병합 표시

## 지원 환경

- macOS
- Linux

## 내부 수집 방식

- macOS: `lsof -nP -iTCP -sTCP:LISTEN -FpcLnTu`
- Linux:
  1. `ss -lntpH`
  2. 실패하면 `lsof -nP -iTCP -sTCP:LISTEN -FpcLnTu` 폴백

## 빠른 시작

### 1) 빌드

```bash
cargo build
```

### 2) 실행

```bash
# 포트 지정 조회
cargo run -- 5432 6379

# 전체 리스닝 포트 조회
cargo run -- --all
```

## CLI 사용법

```text
whichport <port...> [--json] [--verbose]
whichport --all [--json] [--verbose]
```

옵션:

- `--all`: 모든 리스닝 포트 조회
- `--json`: JSON 형식 출력
- `--verbose`: 텍스트 출력에 수집 메타데이터 추가
- `-h`, `--help`: 사용법 출력

주의:

- 포트는 `1..=65535`만 허용됩니다.
- `--all` 없이 포트를 주지 않으면 사용법과 함께 종료됩니다.
- 현재 구현에서 `--help`는 사용법 출력 후 종료 코드 `1`로 종료됩니다.

## 출력 예시

### 텍스트 출력

```bash
cargo run -- 5432 65535 --verbose
```

예시 결과:

```text
meta source: lsof
meta timestamp: 1770834801
meta errors: 0
port 5432: postgres (pid 871, user rexfelix) on [127.0.0.1:5432, [::1]:5432] | PostgreSQL database (high)
port 65535: not listening
```

설명:

- `on [..]`: 같은 리스너(동일 port/pid/command/user)의 엔드포인트 목록
- `role`: 포트/프로세스 이름 기반 추정 결과
- `confidence`: 추정 신뢰도 (`high`, `medium`)

### JSON 출력 (포트 지정)

```bash
cargo run -- --json 5432 65535
```

예시 구조:

```json
{
  "mode": "ports",
  "source": "lsof",
  "timestamp": 1770834801,
  "errors": [],
  "results": [
    {
      "port": 5432,
      "listening": true,
      "listeners": [
        {
          "port": 5432,
          "pid": 871,
          "command": "postgres",
          "user": "rexfelix",
          "endpoint": "127.0.0.1:5432",
          "endpoints": ["127.0.0.1:5432", "[::1]:5432"],
          "role": {
            "description": "PostgreSQL database",
            "confidence": "high"
          }
        }
      ]
    },
    {
      "port": 65535,
      "listening": false,
      "listeners": []
    }
  ]
}
```

### JSON 출력 (전체)

```bash
cargo run -- --json --all
```

`mode`가 `"all"`이고, `results`는 리스너 배열입니다.

## JSON 필드 설명

공통 헤더:

- `mode`: `"ports"` 또는 `"all"`
- `source`: 실제 수집에 사용된 명령 (`ss` 또는 `lsof`)
- `timestamp`: Unix epoch seconds
- `errors`: 수집 중 발생한 오류 목록 (Linux 폴백 이력 포함 가능)

리스너 객체:

- `port`: 포트 번호
- `pid`: 프로세스 ID (`null` 가능)
- `command`: 프로세스명
- `user`: 프로세스 사용자
- `endpoint`: 대표 엔드포인트(하위 호환용)
- `endpoints`: 병합된 전체 바인딩 엔드포인트 목록
- `role.description`: 추정 역할 설명
- `role.confidence`: 추정 신뢰도

## 역할 추정 규칙(요약)

프로세스명 기반으로 우선 추정합니다.

- `postgres`, `redis`, `nginx`, `docker`, `ollama` 등
- `rustrover`, `jetbrains`, `toolbox`, `raycast`, `adobe`, `node` 등

포트 기반 기본 추정:

- `22`, `80`, `443`, `3306`, `5432`, `6379`

매칭되지 않으면 `"Unknown application service"`를 반환합니다.

## 개발

테스트:

```bash
cargo test
```

## 트러블슈팅

- `failed to run lsof`: `lsof`가 설치되어 있는지 확인
- Linux에서 `ss failed ...`: 권한/환경 문제일 수 있으며, 자동으로 `lsof` 폴백 시도
- `invalid port`: 포트 값이 숫자 범위를 벗어났는지 확인

