# MiniVNC 개발 계획 (DEVPLAN)

> TurboVNC 3.3.2의 핵심 기능을 Rust로 재구현하기 위한 분석 및 구현 계획
> 작성일: 2026-11-02 · 분석 대상: `turbovnc/` (TurboVNC 3.3.2)

---

## 0. 요약

- **TurboVNC의 핵심 가치**는 (1) TurboVNC 변형 **Tight 인코딩**(libjpeg-turbo JPEG + zlib 팔레트/모노 + 단색 영역 분할), (2) 지연 업데이트·혼잡 제어·**Continuous Updates/Fence**를 사용한 업데이트 파이프라인, (3) **Automatic Lossless Refresh(ALR)**와 **Interframe Comparison**, (4) VNC/TLS/X.509/PAM 기반 **보안 계층**이다.
- TurboVNC 서버(`Xvnc`)는 X.Org 서버 전체에 RFB DDX를 붙인 구조다. X 서버 자체를 Rust로 다시 만드는 일은 범위 밖이다. MiniVNC는 이를 다음과 같이 대체한다.
  - 서버: **화면 캡처형 RFB 서버**. 기존 X 디스플레이(Xvfb/Xorg)나 Windows 데스크톱을 캡처하고, 입력은 XTest/SendInput으로 주입한다.
  - 뷰어: **네이티브 Rust 뷰어**. Java Swing 뷰어를 대체한다.
- 프로토콜은 **TurboVNC 뷰어·서버와 상호 운용**되는 것을 1차 목표로 한다. 각 단계마다 기존 TurboVNC 바이너리와 교차 테스트한다.
- 범위에서 제외하는 것: X 서버 구현, GLX/DRI3/VirtualGL 통합, TightVNC 파일 전송, RRE/CoRRE/Zlib/ZYWRLE 인코딩, Java 웹 배포.

---

## 1. TurboVNC 소스 구조 분석

### 1.1 디렉터리 개요

| 경로                                         | 내용                                  | MiniVNC 대응                         |
| -------------------------------------------- | ------------------------------------- | ------------------------------------ |
| `common/rfb/rfbproto.h`                      | RFB 프로토콜 상수·메시지 구조체(정본) | `rfb-proto` 크레이트                 |
| `common/rfb/turbovnc_gii.h`                  | GII(확장 입력 장치) 상수              | `rfb-proto` (후순위)                 |
| `common/d3des`                               | VNC 인증용 DES                        | `rfb-auth` (`des` 크레이트)          |
| `common/turbojpeg-jni`                       | Java 뷰어용 TurboJPEG JNI             | `rfb-codec`의 JPEG 백엔드            |
| `unix/Xvnc/programs/Xserver/hw/vnc`          | **RFB 서버 핵심** (약 27,000줄)       | `rfb-server` 크레이트                |
| `unix/Xvnc/programs/Xserver/{dix,fb,mi,...}` | X.Org 서버 본체                       | 대상 아님 (Xvfb 등 외부 X 서버 사용) |
| `unix/libvncauth`, `unix/vncpasswd`          | 비밀번호 파일, OTP, ACL               | `rfb-auth`, `minivnc-passwd`         |
| `unix/vncserver.in`                          | 세션 관리 Perl 스크립트               | `minivnc-session` (후순위)           |
| `unix/turbovncserver*.conf`                  | 서버·보안 설정 파일                   | 설정 모듈 (TOML + 호환 파서)         |
| `java/com/turbovnc/{network,rdr,rfb}`        | 뷰어 프로토콜 코어                    | `rfb-client` 크레이트                |
| `java/com/turbovnc/vncviewer`                | 뷰어 UI (Swing)                       | `minivnc-viewer` 바이너리            |
| `java/com/jcraft/jsch`                       | SSH 터널 (JSch)                       | `russh` 또는 외부 `ssh`              |
| `win/vncviewer/LowLevelHook.cpp`             | Windows 키보드 후킹                   | `minivnc-viewer` (windows 모듈)      |
| `unix/vncviewer/turbovnchelper.c`, `*.m`     | X11/macOS 네이티브 도우미             | 플랫폼 모듈                          |

### 1.2 서버 (`hw/vnc`) 핵심 파일

| 파일                                                 | 줄 수            | 역할                                                          |
| ---------------------------------------------------- | ---------------- | ------------------------------------------------------------- |
| `rfbserver.c`                                        | 3246             | 클라이언트 상태 머신, 메시지 처리, `rfbSendFramebufferUpdate` |
| `draw.c`                                             | 2316             | GC/Render 래핑으로 손상(damage) 영역 추적, CopyRect 영역 병합 |
| `init.c`                                             | 2026             | DDX 진입점, 명령줄 옵션, 화면/입력 장치 초기화                |
| `tight.c`                                            | 1747             | TurboVNC Tight 인코더 (멀티스레드)                            |
| `cutpaste.c`                                         | 1706             | 클립보드 (PRIMARY/CLIPBOARD, Extended Clipboard)              |
| `auth.c` / `authpam.c`                               | 1661 / 219       | 보안 타입, VeNCrypt, VncAuth, OTP, PAM, ACL, 차단             |
| `sprite.c` / `dispcur.c` / `cursor.c`                | 1123 / 540 / 475 | 소프트웨어 커서, 커서 모양 전송                               |
| `sockets.c`                                          | 754              | 리스너, accept, `ReadExact`/`WriteExact`, cork                |
| `input-xkb.c` / `kbdptr.c`                           | 675 / 649        | keysym→keycode 변환, 포인터 이벤트                            |
| `flowcontrol.c`                                      | 664              | Fence, RTT 측정, 혼잡 윈도(TCP Vegas 방식)                    |
| `randr.c`                                            | 661              | 데스크톱 크기 변경, 다중 화면                                 |
| `rfbssl_openssl.c`                                   | 594              | TLS (OpenSSL)                                                 |
| `websockets.c` / `ws_decode.c`                       | 457 / 615        | 같은 포트의 WebSocket(noVNC) 자동 감지                        |
| `translate.c`                                        | 418              | 픽셀 포맷 변환 테이블                                         |
| `zrle*.c`, `hextile.c`, `rre.c`, `corre.c`, `zlib.c` | —                | 기타 인코딩                                                   |
| `qnum_to_xorg{evdev,kbd}.c`                          | —                | QEMU 스캔코드 → X keycode 테이블                              |

---

## 2. TurboVNC 동작 분석

### 2.1 서버 아키텍처

- **단일 스레드 이벤트 루프.** RFB 처리는 X 서버의 poll 루프 안에서 실행된다(`SetNotifyFd(..., rfbSockNotify)`). 지연 업데이트, ALR, 혼잡, 포인터 잠금, 인증 실패 해제 같은 타이머는 X `OsTimer`로 구현되어 있다. 별도 스레드는 Tight 인코딩 작업자뿐이다.
- **I/O.** 논블로킹 소켓이지만 `ReadExact`/`WriteExact`가 `select()`로 최대 20초(`-rfbwait`) 기다린다. 계층은 WebSocket → TLS → raw 순서로 쌓인다. 메시지 묶음마다 `TCP_CORK`를 켜고 끈다.
- **클라이언트 레코드 `rfbClientRec`** (`rfb.h`)
  - 상태
  - 픽셀 포맷과 변환 함수
  - 선호 인코딩, Tight 압축/품질/서브샘플링 수준
  - 영역들: `modifiedRegion`, `copyRegion`(+dx/dy), `requestedRegion`, `cuRegion`, `lossyRegion`, `alrRegion`, `alrEligibleRegion`, `ifRegion`
  - 비교용 프레임버퍼 `compareFB`
  - 각종 `enable*` 능력 플래그
  - 대기 중인 의사 사각형(pseudo-rect) 플래그
  - 혼잡 제어 상태(`congWindow`, `baseRTT`, `pings` 등)
- **상태 머신:** `PROTOCOL_VERSION → SECURITY_TYPE → (TUNNELING_TYPE → AUTH_TYPE) → AUTHENTICATION / TLS_HANDSHAKE → INITIALISATION → NORMAL`

### 2.2 업데이트 파이프라인 (핵심)

1. **손상 추적** (`draw.c`). 모든 그리기 연산의 영역을 각 클라이언트의 `modifiedRegion`에 더한다. CopyArea/CopyWindow는 `copyRegion`과 `(dx, dy)`로 병합한다. CopyRect는 하나만 유지하며, 겹치지 않으면 더 큰 쪽을 남기고 나머지는 modified로 돌린다. PutImage 영역만 ALR 대상(`alrEligibleRegion`)이 된다.
2. **스케줄링.** 손상이 생기면 `rfbDeferUpdateTime` 동안 모았다가 전송한다. 기본값은 40ms이고, `vncserver` 스크립트는 `-deferupdate 1`을 쓴다. `FramebufferUpdateRequest`는 `requestedRegion`에 더해진다. 요청이 non-incremental이면 해당 영역 전체를 modified로 만든다.
3. **`rfbSendFramebufferUpdate` 순서**
   1. 바로 반환하는 경우: sync fence 대기 중, 혼잡 상태(`rfbIsCongested`, ALR 중에는 예외), 데스크톱 크기 변경 중.
   2. ExtendedDesktopSize/DesktopSize를 별도 FBU로 보낸다.
   3. 커서를 처리한다. 클라이언트가 커서를 그리면 스프라이트를 제거하고 모양을 전송한다.
   4. 영역을 계산한다.
      - `copy -= modified`
      - `update = (copy ∪ modified) ∩ requested`
      - 요청 범위 밖의 나머지는 modified로 되돌린다.
   5. 사각형이 100개(`rfbCombineRect`)를 넘으면 경계 상자 하나로 합친다.
   6. **Interframe Comparison**을 한다. 256×256 블록 단위로 이전 프레임 사본과 비교해 바뀐 블록만 보낸다. 모두 같으면 1×1 사각형을 보낸다. CL 5~8에서 자동으로 켜진다.
   7. RTT ping fence를 보낸다.
   8. 다음을 순서대로 기록한다: 커서 모양/위치 → QEMU 키 확인 → LED → ExtMouseButtons 확인 → CopyRect(겹침 순서 보장) → 인코딩된 사각형 → LastRect.
   9. 전송한 뒤 두 번째 ping을 보내고 ALR 타이머를 건다.
4. **ALR** (`-alr sec`)
   - JPEG로 보낸 영역은 `lossyRegion`에, 무손실로 보낸 영역은 lossless로 기록한다.
   - 타이머가 만료되면 `lossyRegion ∩ alrRegion`을 CL1과 `-alrqual` 품질(-1이면 무손실)로 다시 보낸다.
   - ALR 전송은 혼잡 제어를 우회한다.
5. **흐름 제어** (`flowcontrol.c`, TigerVNC 방식)
   - Fence로 RTT를 측정한다.
   - 혼잡 윈도: 초기 16KB, 최소 4KB, 최대 4MB.
   - slow start에서는 윈도를 2배씩 늘린다. RTT가 base RTT보다 25ms 이상 길어지면 Vegas 모드로 바뀐다.
   - Vegas 모드에서 RTT 초과분이 50ms보다 크면 4KB를 줄이고, 5ms 미만이면 8KB, 25ms 미만이면 4KB를 늘린다.
   - 송신 중인 바이트가 윈도를 넘으면 전송을 멈추고, 예상 해소 시각에 다시 시도한다.
6. **Continuous Updates.** Fence와 CU를 모두 지원하는 클라이언트에만 허용한다. 활성화된 `cuRegion`은 요청 없이 계속 전송된다.
7. **멀티스레드 Tight**
   - 사각형을 최대 4개의 수평 띠로 나눈다.
   - 스레드마다 출력 버퍼와 TurboJPEG 핸들을 따로 둔다.
   - **zlib 스트림 4개를 스레드끼리 나눠 가진다.** 디코더 상태와 동기화하기 위해서다.
   - 결과는 스레드 순서대로 기록한다.

### 2.3 RFB 프로토콜 요약

**버전.** 3.3, 3.7, 3.8과 TightVNC의 "t" 확장을 지원한다. 서버는 `RFB 003.008`을 보낸다. 뷰어는 `000.000`을 받으면 UltraVNC Repeater로 처리한다.

**C→S 메시지**

| ID  | 메시지                   | 비고                                                         |
| --- | ------------------------ | ------------------------------------------------------------ |
| 0   | SetPixelFormat           | 20바이트                                                     |
| 1   | FixColourMapEntries      | 미지원. 연결을 끊는다.                                       |
| 2   | SetEncodings             |                                                              |
| 3   | FramebufferUpdateRequest |                                                              |
| 4   | KeyEvent                 |                                                              |
| 5   | PointerEvent             | ExtMouseButtons이면 mask에 0x80이 켜지고 1바이트가 추가된다. |
| 6   | ClientCutText            | 길이가 음수이면 Extended Clipboard                           |
| 150 | EnableContinuousUpdates  |                                                              |
| 248 | Fence                    |                                                              |
| 251 | SetDesktopSize           |                                                              |
| 253 | GII                      |                                                              |
| 255 | QEMU                     | subtype 0 = Extended Key Event (12바이트)                    |

**S→C 메시지**

| ID  | 메시지                 | 비고                                   |
| --- | ---------------------- | -------------------------------------- |
| 0   | FramebufferUpdate      | nRects = 0xFFFF이면 LastRect로 끝난다. |
| 1   | SetColourMapEntries    |                                        |
| 2   | Bell                   |                                        |
| 3   | ServerCutText          |                                        |
| 150 | EndOfContinuousUpdates |                                        |
| 248 | Fence                  |                                        |
| 253 | GII                    |                                        |

**인코딩:** Raw 0, CopyRect 1, RRE 2, CoRRE 4, Hextile 5, Zlib 6, **Tight 7**, ZRLE 16, ZYWRLE 17.

**의사 인코딩**

| 값         | 의미                                 |
| ---------- | ------------------------------------ |
| -768~-763  | 서브샘플링 1X/4X/2X/Gray(/8X/16X)    |
| -512~-412  | 세밀 JPEG 품질 0~100 (TurboVNC 전용) |
| -317       | TightWithoutZlib                     |
| -316       | ExtendedMouseButtons                 |
| -313       | ContinuousUpdates                    |
| -312       | Fence                                |
| -308       | ExtendedDesktopSize                  |
| -307       | DesktopName                          |
| -305       | GII                                  |
| -261       | QEMU LED                             |
| -258       | QEMU Extended Key Event              |
| -256~-247  | CompressLevel 0~9                    |
| -240       | XCursor                              |
| -239       | RichCursor                           |
| -232       | PointerPos                           |
| -224       | LastRect                             |
| -223       | NewFBSize                            |
| -32~-23    | QualityLevel 0~9                     |
| 0x574D5668 | VMware LED                           |
| 0xC0A1E5CE | Extended Clipboard                   |

**SetEncodings 처리 규칙** (`rfbProcessClientNormalMessage`)
- 메시지를 받을 때마다 먼저 초기화한다: CL=1, 서브샘플링 1X, 품질 -1(JPEG 끔), LastRect 끔, 커서 끔.
- 목록에서 처음 나오는 실제 인코딩이 선호 인코딩이 된다. 없으면 Tight다.
- QualityLevel n은 다음 표로 변환된다.
  - 품질 `JPEG_QUAL = {15,29,41,42,62,77,79,86,92,100}`
  - 서브샘플링 `JPEG_SUBSAMP = {1,1,1,2,2,2,0,0,0,0}` (0=1X, 1=4X, 2=2X, 3=Gray)
- 목록의 뒤에 나온 항목이 앞의 값을 덮어쓴다. Java 뷰어는 coarse 품질 → fine 품질 → 서브샘플링 순서로 보낸다.

### 2.4 TurboVNC Tight 인코딩 (가장 중요)

**와이어 형식**

압축 제어 바이트의 하위 4비트는 zlib 스트림 0~3의 리셋 비트다. 상위 4비트는 다음과 같다.

| 상위 비트     | 종류                                    | 뒤따르는 데이터                                |
| ------------- | --------------------------------------- | ---------------------------------------------- |
| `0x8`         | Fill                                    | 픽셀 1개                                       |
| `0x9`         | JPEG                                    | compact length + JFIF                          |
| `0xA` / `0xE` | NoZlib basic (`0xE`는 필터 바이트 포함) |                                                |
| `0b0Fss`      | zlib basic                              | `ss` = 스트림 번호, `F` = 필터 바이트가 따라옴 |

- 필터: 0 = copy, 1 = palette, 2 = gradient.
  - palette는 (색 수 - 1) 바이트와 TPIXEL 팔레트가 앞에 붙는다.
  - 2색이면 1bpp 비트맵(MSB 우선, 행마다 바이트 정렬)이고, 3~256색이면 픽셀당 1바이트다.
- **TPIXEL:** 클라이언트가 depth 24 / 888 포맷이면 픽셀을 3바이트 RGB로 보낸다.
- 필터를 거친 데이터가 12바이트 미만이면 압축 없이 그대로 보낸다. 그 이상이면 compact length와 `Z_SYNC_FLUSH`로 만든 zlib 조각을 보낸다.
- **compact length:** 7+7+8비트, 1~3바이트.
- 제약: 사각형 폭 ≤ 2048.

**TurboVNC 인코더 특성**
- gradient 필터는 보내지 않는다(디코더는 지원해야 한다).
- 리셋 비트를 쓰지 않는다. Java 뷰어는 리셋 비트를 받으면 `Inflater.end()`를 호출하는 버그가 있으므로 **리셋 비트는 절대 보내지 않는다.**
- JPEG는 서버 프레임버퍼에서 바로 압축한다. 따라서 클라이언트 픽셀 포맷과 무관하다. 8bpp 클라이언트에는 JPEG를 쓰지 않는다.

**`tightConf` 표 (내부 레벨 0~3)**

| 레벨 | maxRectSize | maxRectWidth | monoMin | idx/mono/raw zlib | idxMaxColorsDivisor | palMaxColorsWithJPEG |
| ---- | ----------- | ------------ | ------- | ----------------- | ------------------- | -------------------- |
| 0    | 65536       | 2048         | 6       | 0/0/0             | 4                   | 24                   |
| 1    | 65536       | 2048         | 32      | 1/1/1             | 96                  | 24                   |
| 2    | 65536       | 2048         | 32      | 3/3/2             | 96                  | 96                   |
| 3    | 65536       | 2048         | 32      | 7/7/5             | 96                  | 256                  |

**CL → 내부 레벨 매핑**
1. CL 5~8은 0~3에 interframe comparison을 켠 것과 같다.
2. JPEG를 쓰면 1~2로 제한한다.
3. JPEG를 쓰지 않으면 최대 1이다.
4. CL 9는 항상 3이다.

**뷰어 프리셋** (`OptionsDialog`)

| 프리셋                | 설정                  |
| --------------------- | --------------------- |
| Perceptually Lossless | JPEG Q95, 1X, CL1     |
| Medium                | Q80, 2X, CL6          |
| Low                   | Q30, 4X, CL7          |
| Lossless Tight        | JPEG 끔, CL0 (NoZlib) |
| Lossless Tight + Zlib | JPEG 끔, CL6          |

**부호화 알고리즘** (`SendRectEncodingTight` → `SendRectSimple` → `SendSubrect`)

1. **단색 영역 탐색.** LastRect가 켜져 있고 면적이 4096 이상일 때만 한다.
   - 16×16 타일을 `CheckSolidTile`로 검사한다.
   - `FindBestSolidArea`로 타일 단위로 확장하고, `ExtendSolidArea`로 픽셀 단위로 확장한다.
   - 면적이 2048 미만이면 버린다. 단, 사각형 전체가 단색이면 그대로 쓴다.
   - 위쪽 띠, 왼쪽(재귀), Fill, 오른쪽(재귀), 아래쪽(재귀) 순서로 보낸다.
   - 이렇게 나누면 사각형 수를 미리 알 수 없다. 그래서 nRects = 0xFFFF로 보내고 LastRect로 끝낸다.
2. **`SendRectSimple`.** 사각형을 폭 2048, 높이 `maxRectSize/폭` 이하로 자른다.
3. **`SendSubrect`.** 색 수를 센다(`FillPalette*`, 256버킷 해시, 빈도 내림차순).
   - 팔레트 상한: JPEG를 쓰지 않으면 `w*h/idxMaxColorsDivisor`, 쓰면 `palMaxColorsWithJPEG`.
   - 1색 → Fill
   - 2색 → Mono. 많은 색이 배경(0)이다.
   - 3~상한 → Indexed
   - 상한 초과 → JPEG(품질 ≠ -1일 때) 또는 Full-color(copy 필터 + TPIXEL)
   - 회색조 JPEG 모드에서는 항상 JPEG를 쓴다.
4. **zlib.** 클라이언트마다 스트림 4개를 계속 유지한다(windowBits 15, memLevel 9). 레벨이 바뀌면 `deflateParams`를 호출한다. 스트림은 round-robin으로 돌려 쓴다.

### 2.5 보안

| 이름      | RFB 타입 / VeNCrypt 서브타입 | 내용                                                                                                          |
| --------- | ---------------------------- | ------------------------------------------------------------------------------------------------------------- |
| None      | 1                            | 인증 없음                                                                                                     |
| VncAuth   | 2                            | 16바이트 챌린지를 DES로 암호화. 키는 비밀번호를 0으로 채운 8바이트이며 비트 순서가 뒤집힌다.                  |
| Tight     | 16                           | 터널 목록(NOTUNNEL) → 인증 능력 목록 → 상호작용 능력. TurboVNC 뷰어는 Tight를 항상 우선 선택한다.             |
| VeNCrypt  | 19                           | 버전 0.2, 서브타입 Plain 256, TLSNone 257, TLSVnc 258, TLSPlain 259, X509None 260, X509Vnc 261, X509Plain 262 |
| UnixLogin | 129 (Tight 인증 능력)        | Plain과 같은 형식. PAM 사용                                                                                   |

- **TLS\*** 는 익명 DH(aNULL)를 쓴다. **X509\*** 는 인증서를 쓴다.
- **비밀번호 파일:** 8바이트(전체 제어) 또는 16바이트(전체 제어 + view-only)다. 각 비밀번호를 고정 키 `{23,82,107,6,35,78,88,7}`로 DES-ECB 암호화해 저장한다.
- **OTP**는 일회용 비밀번호다. 성공하면 사용된 것으로 처리해 다시 쓸 수 없다.
- **ACL:** `enable-user-acl`이 켜져 있으면 세션 소유자 외의 사용자도 허용할 수 있다.
- **무차별 대입 방지:** 호스트당 5회 실패하면 10초 동안 차단한다. 이후 실패할 때마다 차단 시간이 2배가 된다.
- **보안 설정 파일** (`turbovncserver-security.conf`)
  - `permitted-security-types`
  - `no-remote-resize`
  - `no-reverse-connections`
  - `no-remote-connections`
  - `no-clipboard-send` / `no-clipboard-recv`
  - `max-desktop-size`
  - `enable-user-acl`
  - `pam-service-name`
  - `max-idle-timeout`
  - 기타 항목
- **원본의 약점(개선 대상):** `vncRandomBytes`는 시간과 pid로 시드한 `random()`이다. Rust에서는 CSPRNG를 사용한다.

### 2.6 입력

- **keysym 경로** (`kbdptr.c`, `input-xkb.c`)
  - 현재 XKB 맵에서 keysym을 keycode로 찾는다.
  - 필요하면 가짜 Shift/Level3 이벤트를 삽입한다.
  - 맵에 없는 keysym은 사용하지 않는 keycode에 동적으로 할당한다.
- **QEMU Extended Key Event**
  - XT 스캔코드("qnum")를 evdev/kbd keycode로 바꾼다(`qnum_to_xorgevdev.c`).
  - 뷰어는 서버가 -258 의사 사각형과 LED 상태를 모두 보내야 이 경로를 쓴다.
- **포인터:** 버튼은 9개이고, ExtMouseButtons를 쓰면 15개다(4/5는 휠). 드래그 중에는 해당 클라이언트가 포인터를 독점한다(포인터 잠금).
- **로컬 패치** (`turbovnc/MEMO.md`, CConn.java)
  - Windows 한글 키보드에서 한/영 키를 qnum 0x72 (`XK_Hangul`, 0xff31)로 보낸다.
  - 한자 키를 qnum 0x71 (`XK_Hangul_Hanja`, 0xff34)로 보낸다.
  - 서버에서는 각각 evdev keycode 130과 131이 된다.
  - **MiniVNC 뷰어는 이 동작을 기본으로 내장한다.**

### 2.7 기타 기능

- **클립보드:** Extended Clipboard를 사용한다(UTF-8, zlib으로 압축한 provide). 데이터 크기 상한은 기본 1MB다. view-only 클라이언트에는 막는다.
- **커서:** RichCursor/XCursor로 클라이언트 측 커서를 쓴다. 지원하지 않는 클라이언트에는 서버 측 소프트웨어 커서를 그린다.
- **RandR / 다중 화면**
  - `SetDesktopSize`로 크기를 바꾸면 프레임버퍼를 다시 할당하고 모든 클라이언트를 갱신한다.
  - ExtendedDesktopSize에 화면 목록을 넣어 보낸다.
  - 첫 FBU에는 항상 ExtDesktopSize를 넣는다(TigerVNC 호환).
- **WebSocket:** 같은 포트에서 처음 4바이트를 엿봐 판별한다("GET " 또는 TLS ClientHello). RFC 6455만 지원하며, subprotocol은 binary 또는 base64다.
- **역방향 연결:** 서버가 뷰어의 리슨 포트(기본 5500)로 접속한다. UltraVNC repeater의 `ID:n` 형식도 지원한다.
- **세션 스크립트** (`vncserver`)
  - 빈 디스플레이 번호를 찾고 xauth 쿠키를 만든다.
  - Xvnc를 실행하고 `xstartup.turbovnc`를 실행한다.
  - `-kill`과 `-list`를 지원한다.
  - 필요하면 OTP를 만들고 noVNC를 띄운다.

### 2.8 Java 뷰어 구조

- **계층:** `network`(소켓, TLS 엔진) → `rdr`(버퍼링된 빅엔디언 스트림) → `rfb`(상태 머신, 리더/라이터, 디코더, 보안) → `vncviewer`(CConn 컨트롤러, Swing UI)
- **스레드:** 연결마다 RFB 스레드 하나가 메시지를 읽고 바로 디코딩한다. EDT는 그리기만 한다.
- **JPEG:** TurboJPEG JNI가 프레임버퍼 배열에 직접 디코딩한다.
- **성능 기법**
  - **요청 파이프라이닝:** FBU 시작 시점에 바로 다음 요청을 보낸다.
  - 첫 업데이트가 끝나면 Fence(SyncNext)를 보낸다. 응답이 오면 Continuous Updates를 켠다.
  - 픽셀 포맷은 fence payload에 실어 동기적으로 바꾼다.
- **키보드:** 두 가지 경로가 있다.
  - QEMU 스캔코드 경로: Windows는 `scancode=`, X11은 `rawCode=`를 테이블로 변환한다.
  - keysym 경로: VK 코드나 Unicode를 keysym으로 바꾼다.
  - 보정 로직
    - AltGr 감지: LCtrl 다음 50ms 안에 RAlt가 오면 AltGr로 본다.
    - 포커스를 잃으면 눌린 키를 모두 해제한다.
    - LED 상태를 동기화한다.
  - Windows `WH_KEYBOARD_LL` 후킹으로 Win 키, Alt+Tab 등을 가로챈다.
- **옵션:** 약 80개다. `-Name=value` 형식을 쓰고, 연결 정보 파일(.vnc)과 호스트별 설정을 저장한다.
- **알려진 버그(재현하지 않을 것)**
  - `CSecurityPlain`이 길이를 문자 수로 보내 UTF-8 자격 증명이 잘린다.
  - `LowLevelHook`의 `MakeLParam`이 스캔코드를 지워 버린다.

---

## 3. MiniVNC 목표와 범위

### 3.1 목표

1. **TurboVNC와 상호 운용**
   - MiniVNC 서버 ↔ TurboVNC 뷰어(Java)
   - TurboVNC 서버(Xvnc) ↔ MiniVNC 뷰어
   - 둘 다 Tight + JPEG, 모든 프리셋에서 정상 동작해야 한다.
2. **성능:** 같은 조건에서 TurboVNC 대비 인코딩 처리량 80% 이상, 대역폭 ±10% 이내.
3. **안전성:** `unsafe`를 최소화한다(FFI 경계에 한정). 모든 입력 파서는 길이를 검증하고 fuzz 테스트를 한다.
4. **크로스 플랫폼:** 뷰어는 Windows/Linux(/macOS), 서버는 Linux(X11)를 먼저 하고 Windows는 선택 사항이다.

### 3.2 기능 우선순위

| 우선순위     | 서버                                                                                                                                                                          | 뷰어                                                                                       |
| ------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------ |
| **P0 (MVP)** | RFB 3.3/3.7/3.8, None/VncAuth, Raw/CopyRect/**Tight(+JPEG)**, LastRect, DesktopSize, Key/Pointer, X11 캡처 + XTest                                                            | 같은 프로토콜, Tight/Raw/CopyRect 디코딩, 창 표시, 키(keysym)/마우스                       |
| **P1**       | Tight 보안 타입, VeNCrypt/TLS/X509, Fence/CU/흐름 제어, ExtDesktopSize/SetDesktopSize, QEMU 키 이벤트 + LED, 커서 모양, Extended Clipboard, 멀티스레드 Tight, Interframe, ALR | 같은 기능, QEMU 스캔코드 경로(**한/영·한자 포함**), 스케일링/전체 화면, 클립보드, 옵션/CLI |
| **P2**       | ZRLE/Hextile, WebSocket(noVNC), PAM/Plain/UnixLogin, OTP/ACL, 보안 설정 파일, 역방향 연결, 세션 관리 도구, 다중 화면                                                          | ZRLE/Hextile, SSH 터널(`-via`), 리슨 모드, 다중 모니터 span, 프로파일러                    |
| **제외**     | RRE/CoRRE/Zlib/ZYWRLE, 파일 전송, GII(필요할 때만), GLX/DRI3/VirtualGL 통합                                                                                                   | Session Manager(후순위), Java 웹 배포                                                      |

---

## 4. 아키텍처 설계

### 4.1 서버 구조: 캡처형 서버

Xvnc는 X 서버의 그리기 연산을 가로채 정확한 손상 영역을 얻는다. MiniVNC는 X 서버를 직접 구현하지 않는다. 대신 **`FrameSource` 추상화**를 둔다.

```text
            ┌──────────────────────── minivnc-server ────────────────────────┐
 Xvfb/Xorg ─┤ FrameSource (x11: XShm + XDamage) ──► Framebuffer + DamageQueue │
 Windows   ─┤ FrameSource (DXGI Desktop Duplication)                         │
            │ InputSink (x11: XTest + XKB keymap / windows: SendInput)        │
            └───────────────┬────────────────────────────────────────────────┘
                            ▼
                     rfb-server (플랫폼 독립)
   ┌──────────┬───────────────┬────────────────┬──────────────┬────────────┐
   │ Listener │ ClientSession │ UpdateScheduler│ Encoder pool │ FlowControl│
   │ TCP/UDS/ │ (상태 머신,     │ (영역 연산,     │ (Tight 멀티   │ (Fence/RTT,│
   │ WS 감지   │  보안, 메시지)  │  defer, ALR,   │  스레드, ZRLE)│  혼잡 윈도)  │
   │          │               │  interframe)   │              │            │
   └──────────┴───────────────┴────────────────┴──────────────┴────────────┘
```

- **X11 백엔드** (`x11rb`)
  - 손상 영역: `XDamage` (`DamageReportRawRectangles` 또는 `NonEmpty` + 영역 조회)
  - 픽셀 읽기: `MIT-SHM` `ShmGetImage`
  - 커서: `XFixes` 커서 이미지와 변경 이벤트
  - 해상도 변경: `RandR`
  - 클립보드: X 선택(selection)
  - 입력: `XTest`. keysym→keycode 변환은 `input-xkb.c` 로직을 이식한다.
- **헤드리스 세션:** `minivnc-session`이 `Xvfb :N`(또는 Xorg dummy 드라이버)을 띄우고 서버를 연결한다. TurboVNC의 `vncserver` 사용 경험을 재현하는 것이 목적이다.
- **CopyRect 감지:** 캡처형 서버는 copy 연산을 직접 알 수 없다. P2에서 스크롤 감지 휴리스틱(행 해시 비교)을 넣는다. 그전까지 CopyRect는 지원 플래그만 유지한다.
- **Interframe comparison의 중요성:** XDamage는 실제 변경보다 넓은 영역을 보고하는 경우가 많다. 그래서 TurboVNC보다 더 중요하다. 캡처형 서버에서는 **항상 켜는 것**을 기본값으로 검토한다.

### 4.2 뷰어 구조

```text
  RFB 스레드 (blocking I/O)                     UI 스레드 (winit 이벤트 루프)
 ┌─────────────────────────────────┐           ┌─────────────────────────────────┐
 │ rfb-client::Connection          │  damage   │ Window + softbuffer/wgpu 블릿   │
 │  ├ 핸드셰이크/보안 상태 머신    │ ────────► │ 스케일링, 전체 화면, 커서       │
 │  ├ MessageReader → Decoders     │           │ 키보드: scancode→qnum + keysym  │
 │  └ 공유 Framebuffer에 직접 쓰기 │ ◄──────── │ 마우스, 휠, 클립보드(arboard)   │
 └─────────────────────────────────┘  input    └─────────────────────────────────┘
                                      commands
```

- 프레임버퍼는 `Arc<Mutex<Framebuffer>>`(또는 이중 버퍼)로 공유한다. 손상 사각형은 채널로 UI 스레드에 알린다(`EventLoopProxy`).
- 디코딩은 우선 단일 스레드로 한다. 필요하면 P2에서 TigerVNC DecodeManager처럼 사각형별 병렬 디코딩을 넣는다. 서로 다른 zlib 스트림을 쓰는 사각형만 병렬화할 수 있다.

### 4.3 Cargo 워크스페이스

```text
minivnc/
├── Cargo.toml                 # [workspace]
├── crates/
│   ├── rfb-proto/             # 상수, 메시지 타입, PixelFormat, 직렬화/파싱 (I/O 없음)
│   ├── rfb-codec/             # 인코더/디코더: raw, copyrect, tight, zrle, hextile; JPEG 추상화
│   ├── rfb-auth/              # VncAuth DES, 비밀번호 파일, VeNCrypt, TLS 래퍼, PAM(옵션)
│   ├── rfb-region/            # 사각형 영역(Region) 연산: union/intersect/subtract/translate
│   ├── rfb-server/            # 플랫폼 독립 서버 엔진
│   ├── rfb-client/            # 플랫폼 독립 클라이언트 엔진
│   ├── capture-x11/           # FrameSource/InputSink: X11
│   └── capture-windows/       # FrameSource/InputSink: Windows (P2)
├── bins/
│   ├── minivnc-server/        # 서버 실행 파일 (CLI, 설정 파일)
│   ├── minivnc-viewer/        # 뷰어 실행 파일
│   ├── minivnc-passwd/        # vncpasswd 대응 (TurboVNC 비밀번호 파일 호환)
│   └── minivnc-session/       # vncserver 스크립트 대응 (P2)
├── tools/
│   ├── rfb-snapshot/          # 헤드리스 접속 후 화면을 PNG로 저장 (상호 운용 검증용)
│   ├── rfb-record/            # RFB 스트림 녹화 (Xvnc -capture 대응)
│   └── rfb-bench/             # 녹화 재생 벤치마크 (Java -bench 대응)
└── tests/interop/             # TurboVNC 상호 운용 테스트 스크립트
```

### 4.4 핵심 타입과 트레이트 (초안)

```rust
// rfb-proto
pub struct PixelFormat { bpp: u8, depth: u8, big_endian: bool, true_colour: bool,
                         max: [u16; 3], shift: [u8; 3] }
pub enum ClientMessage { SetPixelFormat(PixelFormat), SetEncodings(Vec<i32>),
                         FramebufferUpdateRequest { incremental: bool, rect: Rect },
                         KeyEvent { down: bool, keysym: u32 },
                         QemuKeyEvent { down: bool, keysym: u32, keycode: u32 },
                         PointerEvent { mask: u16, x: u16, y: u16 }, ClientCutText(CutText),
                         EnableContinuousUpdates { enable: bool, rect: Rect },
                         Fence { flags: u32, data: Vec<u8> },
                         SetDesktopSize { w: u16, h: u16, screens: Vec<Screen> }, /* ... */ }
// 파서는 "추가 바이트 필요" 상태를 반환하는 증분형으로 만든다 → blocking/async 양쪽에서 재사용
pub fn parse_client_message(buf: &[u8], ctx: &ParseCtx) -> Result<Parsed<ClientMessage>, ProtoError>;

// rfb-codec
pub trait Encoder: Send {
    fn encode(&mut self, fb: &FrameView, rect: Rect, out: &mut Vec<u8>) -> EncodeStats;
}
pub trait Decoder: Send {
    fn decode(&mut self, rect: Rect, input: &mut dyn ByteSource, fb: &mut FramebufferMut) -> Result<()>;
}
pub trait JpegBackend { fn compress(..); fn decompress(..); } // turbojpeg / jpeg-encoder(순수 Rust)

// capture-*
pub trait FrameSource {
    fn geometry(&self) -> Geometry;
    fn wait_damage(&mut self, timeout: Duration) -> Result<Region>;
    fn read(&mut self, region: &Region, fb: &mut Framebuffer) -> Result<()>;
    fn cursor(&mut self) -> Option<CursorImage>;
    fn resize(&mut self, layout: &ScreenLayout) -> Result<ResizeResult>;
}
pub trait InputSink {
    fn key_sym(&mut self, down: bool, keysym: u32);
    fn key_code(&mut self, down: bool, qnum: u32, keysym: u32);
    fn pointer(&mut self, x: u16, y: u16, buttons: u16);
    fn set_clipboard(&mut self, text: &str);
}
```

### 4.5 기술 선택

| 영역              | 선택                                                                   | 이유 / 대안                                                                                                             |
| ----------------- | ---------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| 비동기 I/O (서버) | `tokio`                                                                | 클라이언트 다수, 타이머(defer/ALR/congestion)를 자연스럽게 처리한다. 인코딩은 `spawn_blocking` 또는 `rayon`으로 돌린다. |
| I/O (뷰어)        | 표준 blocking 스레드                                                   | 원본 구조와 같고 단순하다.                                                                                              |
| zlib              | 압축: C zlib(`libz-sys` 정적 빌드) 직접 래핑, 해제: `flate2`          | 영구 스트림, `Z_SYNC_FLUSH`, `deflateParams`가 필요하다. flate2의 `set_level`은 이 용도로 동작하지 않는다(M2 참고). |
| JPEG              | 기본 `jpeg-encoder`/`zune-jpeg`(순수 Rust), feature `turbojpeg`         | 빌드 의존성 없이 동작한다. libjpeg-turbo는 TurboVNC와 같은 출력을 내며, CMake와 NASM이 필요하다.                       |
| DES               | `des` 크레이트                                                         | VNC 키 비트 반전을 직접 처리해야 한다.                                                                                  |
| TLS               | `openssl` (서버는 `tokio-openssl`), Windows는 `vendored`               | 익명 DH(aNULL)와 X.509를 한 라이브러리로 처리한다. rustls는 aNULL을 지원하지 않는다. Linux는 시스템 libssl을 쓴다.     |
| X11               | `x11rb` (shm, damage, xfixes, xtest, randr, xkb)                       | 순수 Rust이고 확장을 모두 지원한다.                                                                                     |
| 창/그리기         | `winit` + `softbuffer` (필요 시 `wgpu`로 스케일링)                     | 가볍고 크로스 플랫폼이다.                                                                                               |
| 클립보드          | `arboard`                                                              | Windows/X11/macOS를 지원한다.                                                                                           |
| GUI 대화상자      | `egui` (옵션/접속/비밀번호)                                            | 즉시 모드 GUI라 간단하다.                                                                                               |
| SSH               | 1단계는 외부 `ssh` 실행, 2단계는 `russh`                               | JSch와 같은 기능 범위                                                                                                   |
| 로깅              | `tracing`                                                              | `-loglevel`에 대응한다.                                                                                                 |
| CLI               | `clap` + TurboVNC 스타일(`-Name=value`) 호환 파서                      | 기존 스크립트와 호환된다.                                                                                               |
| PAM               | `pam` 0.7 크레이트                                                     | Linux 전용. 0.8은 bindgen(libclang)이 필요해 쓰지 않았다.                                                              |

### 4.6 영역(Region) 연산

TurboVNC는 X 서버의 `RegionRec`(pixman band 구조)에 크게 의존한다. `rfb-region` 크레이트는 다음을 구현한다.
- 표현: y-band로 정렬한 사각형 목록(pixman 방식)
- 연산: `union`, `intersect`, `subtract`, `translate`, `extents`, `num_rects`, `is_empty`, `iter`
- 사각형이 너무 많을 때 합치는 정책(`rfbCombineRect=100` → 경계 상자)
- 속성 기반 테스트(`proptest`): 비트맵 기반 참조 구현과 결과를 비교한다.

---

## 5. 단계별 구현 계획

각 마일스톤에는 **완료 기준(DoD)**이 있다. 상호 운용 검증에는 TurboVNC 3.3.2 바이너리(Java 뷰어, Xvnc)를 사용한다.

### M0. 기반 구축 (1주)
- [x] Cargo 워크스페이스와 크레이트 골격, `rustfmt`/`clippy`/CI(GitHub Actions: Windows + Ubuntu)를 구성한다.
  - 워크스페이스에는 M0 크레이트(`rfb-proto`, `rfb-region`)만 둔다. 나머지 크레이트는 해당 마일스톤에서 추가한다.
- [x] `rfb-proto`: 모든 상수(§2.3), `PixelFormat`, `Rect`, 빅엔디언 읽기/쓰기 유틸, compact length를 만든다.
  - 상수 이름은 C 이름을 기계적으로 바꾼다(`rfbSecTypeNone` → `SEC_TYPE_NONE`, `sig_rfbAuthVNC` → `SIG_AUTH_VNC`, `sz_rfbRectangle` → `SZ_RECTANGLE`).
  - TightVNC 파일 전송 상수(`rfbFile*`)와 `rfbProtocolVersionFormat`은 옮기지 않는다. `ENCODING_DESKTOP_NAME`(-307)은 `RFB.java`에만 있어 따로 추가했다.
- [x] `rfb-region`: 영역 연산과 속성 테스트를 만든다.
- [x] 라이선스를 결정한다(§8). README와 DEVPLAN을 정리한다.
- **DoD:** `cargo test` 통과, 상수 표가 `rfbproto.h`와 1:1로 일치(자동 대조 테스트).
  - 대조 테스트는 저장소에 넣은 헤더 사본(`crates/rfb-proto/tests/data/rfbproto.h`)을 읽는다. `turbovnc/`는 저장소 밖을 가리키는 링크라 CI에서 읽을 수 없기 때문이다.

### M1. 프로토콜 코어 + Raw (2주)
- [x] 메시지 파서/직렬화기(C→S, S→C 전부)와 증분 파싱을 만든다.
  - GII 메시지는 내용을 해석하지 않고 틀(endian, subtype, payload)만 읽는다.
  - 수신 상한(기본 1MB)을 넘는 cut text는 연결 오류로 처리한다. TurboVNC는 상한까지만 받고 나머지를 버린다.
- [x] 핸드셰이크 3.3/3.7/3.8, 보안 None/VncAuth를 구현한다.
  - DES 비트 반전, 비밀번호 파일 호환
  - `minivnc-passwd`: TurboVNC `~/.vnc/passwd`와 상호 호환 (`[-v] [FILE]`, `-f`. OTP/ACL 옵션은 M6)
  - 챌린지는 OS CSPRNG(`getrandom`)로 만든다.
  - TurboVNC는 Tight 보안 타입(16)을 항상 광고하지만, MiniVNC는 M6 전까지 광고하지 않는다.
- [x] `rfb-server`
  - 리스너(TCP 5900+N)와 `ClientSession` 상태 머신
  - SetPixelFormat, 픽셀 변환(identity / 3-테이블 / 16bpp 단일 테이블, 8bpp 컬러맵 클라이언트는 BGR233)
  - Raw 인코딩 (SetEncodings와 무관하게 Raw만 보낸다)
  - 업데이트: `modified ∩ requested`를 보내고, 사각형이 100개를 넘으면 경계 상자로 합친다. 지연 업데이트·shared/exclusive 정책·입력 주입은 M3에서 한다.
- [x] `rfb-client`: 핸드셰이크, Raw 디코딩, 프레임버퍼 관리
- [x] 테스트용 `FrameSource`: 합성 패턴과 PNG 시퀀스 (`minivnc-server -source pattern|png:PATH`)
- **DoD**
  - TurboVNC Java 뷰어가 MiniVNC 서버(합성 화면, Raw, VncAuth)에 접속해 화면을 표시한다.
  - MiniVNC 클라이언트(헤드리스)가 Xvnc에 접속해 프레임을 PNG로 저장한다.
- **검증 기록 (2026-11-02)**
  - Java 뷰어(TurboVNC 3.3.2 소스 빌드)는 `-PasswordFile`로 `minivnc-passwd`가 만든 파일을 써서 VncAuth에 성공했다. 기본 포맷(rgb888)과 `-Colors 256`(rgb332)에서 합성 화면과 움직이는 사각형이 정상 표시됐다.
  - `rfb-snapshot`이 WSL의 TurboVNC 3.3.1 Xvnc(`-depth 24`, `-depth 16`)에 VncAuth로 접속해 800x600 PNG를 저장했다. 두 깊이의 결과는 픽셀 단위로 같았다. 틀린 비밀번호는 3.8 실패 사유와 함께 거부됐다.
  - GitHub에 3.3.2 릴리스 패키지가 없어 Xvnc는 3.3.1을 썼다. sudo 없이 `.deb`를 풀어 실행했고, `xkbcomp`는 `-xkbcompdir`로 지정했다.

### M2. Tight 인코딩 (3주) — 핵심
- [x] **디코더** (먼저 만들어 인코더 검증에 쓴다)
  - Fill/JPEG/basic
  - copy, palette(1bpp/8bpp), gradient 필터
  - 영구 zlib 스트림 4개와 리셋 비트, NoZlib, TPIXEL
- [x] **인코더**
  - `tightConf`와 CL 매핑
  - 단색 영역 분할(`CheckSolidTile`/`FindBestSolidArea`/`ExtendSolidArea`)
  - `SendRectSimple` 분할
  - `FillPalette`(빈도 정렬 해시 팔레트)
  - Mono/Indexed/FullColor/JPEG 분기
  - TPIXEL `Pack24`
  - zlib 스트림 round-robin
  - LastRect(0xFFFF)
  - 원본의 세부 동작을 그대로 따른다.
    - `FillPalette`에서 c1의 첫 픽셀은 개수에 넣지 않는다.
    - zlib 스트림 번호는 최상위 사각형마다 0부터 다시 시작한다.
    - 회색조 JPEG 모드에서는 휘도 값으로 단색 영역을 찾으므로 사실상 분할이 일어나지 않는다.
    - 8bpp 클라이언트에도 JPEG를 보낸다.
- [x] JPEG 백엔드: 서브샘플링 매핑 {444, 420, 422, GRAY}, 회색조 Fill 변환
  - 기본은 순수 Rust(`jpeg-encoder`/`zune-jpeg`)이고, `rfb-codec/turbojpeg` feature를 켜면 libjpeg-turbo를 쓴다(CMake와 NASM 필요).
- [x] zlib: 레벨 변경을 `deflateParams`로 하므로 C zlib(`libz-sys` 정적 빌드)를 직접 감싼다(`tight/zstream.rs`, 크레이트 안의 유일한 `unsafe`).
  - flate2는 호출 사이에 `next_out`을 비워 `set_level`이 실패한다.
  - 스트림을 리셋하면 Java 뷰어 버그를 건드린다.
  - inflate는 flate2를 쓴다.
- [x] 품질 의사 인코딩 처리: coarse/fine/subsamp, 순서에 따라 덮어쓰기
  - Raw/Tight 외의 인코딩(Hextile, ZRLE 등)은 M8 전까지 건너뛴다.
- [x] CopyRect 인코딩/디코딩(와이어 형식). 서버는 아직 CopyRect를 만들지 않는다(§4.1).
- **DoD**
  - Java 뷰어 5개 프리셋 전부에서 화면이 깨지지 않는다.
  - Xvnc → MiniVNC 클라이언트에서 디코딩 결과가 무손실 모드에서 서버 화면과 비트 단위로 같다.
  - 같은 입력에서 TurboVNC 대비 출력 크기 ±10% 이내(녹화 데이터 기준).
- **검증 기록 (2026-11-02)**
  - 시험 이미지는 1024x768 3종이다: 스크린숏형(ui), 사진형(photo), 혼합(mixed). Xvnc에는 `xwud`로, MiniVNC에는 PNG 소스로 띄웠다. `rfb-snapshot -preset`으로 Java 뷰어와 같은 SetEncodings를 보내 비교했다. 녹화 도구(M7)가 없어 "녹화 데이터" 대신 이 방법을 썼다.
  - **비트 일치**: 무손실 프리셋(lossless, lossless-zlib, raw)에서 이미지 3종 모두 원본과 0픽셀 차이였다. Xvnc → MiniVNC 클라이언트와 MiniVNC 서버 → MiniVNC 클라이언트 양쪽 다 해당한다.
  - **Java 뷰어**: 5개 프리셋 모두 정상 표시됐다. 뷰어 창을 캡처해 비교한 결과, 무손실 2종은 원본과 0픽셀 차이였고 JPEG 3종의 평균 오차는 Xvnc와 비슷했다.
  - **출력 크기**(MiniVNC / TurboVNC 3.3.1): 순수 Rust JPEG에서 0.989~1.052, `turbojpeg`에서 0.989~1.022였다.

### M3. 실제 화면 서버 — X11 (2주)
- [x] `capture-x11`
  - XShm 캡처(없으면 GetImage), XDamage 손상 영역(DELTA_RECTANGLES + Subtract)
  - 지오메트리는 연결 시 루트 창 크기로 정한다. RandR 크기 변경은 DesktopSize와 함께 M5에서 한다.
  - XFixes 커서: 클라이언트 커서 인코딩이 M5라서 지금은 커서 이미지를 프레임버퍼에 합성한다(포인터 위치 10ms 폴링).
  - XTest 입력과 keysym→keycode 변환(`input-xkb.c` 로직: Shift/Level3 보정, 동적 keysym 할당, 눌린 키 추적)
    - 키맵 로직(`keymap.rs`)은 플랫폼 독립이라 Windows에서도 모의 키맵으로 테스트한다.
    - X 서버 밖에서 동작하므로 원본과 다른 점이 있다. XTest 입력 뒤 코어 키보드의 `lookup_mods`가 갱신되지 않아(Xvfb에서 확인) 유효 수정자(`mods`)를 쓴다.
  - x11rb(순수 Rust)는 Linux 추상 소켓에 접속하지 않으므로 직접 시도한다. WSLg처럼 `/tmp/.X11-unix`가 읽기 전용이면 Xvfb는 추상 소켓만 연다.
- [x] **업데이트 스케줄러**
  - modified/requested 영역 관리, 지연 업데이트(`-deferupdate`, 기본 1ms: `vncserver` 스크립트 값)
  - 100개 초과 사각형 병합
  - non-incremental 요청 처리
- [x] 다중 클라이언트: shared/exclusive 정책(`-alwaysshared`, `-nevershared`, `-disconnect`/`-dontdisconnect`), 포인터 잠금(`-pointerlocktimeout`, 기본 3000ms), 첫 클라이언트 접속 시 눌린 키 해제
- [x] `minivnc-server` CLI: `-rfbport`, `-rfbauth`, `-securitytypes`, `-deferupdate`, `-localhost`, `-viewonly`, `-display`
- **DoD:** Xvfb + xfce4 세션을 Java 뷰어로 일상적으로 쓸 수 있다(타이핑, 창 이동, 동영상 재생).
- **검증 기록 (2026-11-02)**
  - 환경: WSL Ubuntu 26.04, Xvfb 1280x800x24 + xfce4, Windows의 TurboVNC Java 뷰어(Tight + JPEG Q95 1X CL1). 입력은 시험용 RFB 클라이언트로 KeyEvent/PointerEvent를 보내고 결과를 뷰어 창 캡처와 터미널 출력으로 확인했다.
  - **타이핑**: 터미널에 입력한 문자열이 파일로 그대로 기록됐다. 출력 가능한 ASCII 전체에 대해 Shift를 보내는 경우(뷰어 방식)와 보내지 않는 경우 모두 맞았고, `é ü ß æ ñ €`는 동적 keysym 할당으로 입력됐다. Ctrl+D 같은 수정자 조합과 명령 실행도 정상이었다.
  - **창 이동**: 제목 표시줄을 끌어 창을 옮겼고, 남는 흔적 없이 다시 그려졌다.
  - **동영상**: mpv(640x360, 30fps 시험 영상, `--vo=x11`) 재생 중 클라이언트가 초당 23.5개 업데이트(1.2MB/s)를 받았다. 이때 서버 CPU 사용률은 약 17%였다.

### M4. MiniVNC 뷰어 MVP (3주)
- [x] `winit` + `softbuffer` 창, 창 크기 = 원격 크기, 스크롤
  - RFB 스레드가 32bpp RGB888을 요청해 디코딩하고, 바뀐 사각형만 공유 화면 버퍼에 복사한 뒤 창에 알린다. 알림은 하나만 쌓이므로 업데이트가 몰려도 창은 최신 화면을 한 번 그린다.
  - 다시 그릴 때는 보이는 영역 전체를 복사한다. 손상 영역만 블릿하는 최적화는 필요해지면 한다.
  - 창은 원격 크기보다 커지지 않는다. 작아지면 스크롤바가 생기고, 끌어서 화면을 옮긴다.
  - 원격 커서는 서버가 그린다(커서 인코딩은 M5). 로컬 커서는 TurboVNC 뷰어처럼 작은 점으로 바꾼다.
- [x] 마우스: 버튼 1~3, 휠 4/5(세로)와 6/7(가로, 클릭 단위로 누적), 창 좌표 + 스크롤 오프셋 → 원격 좌표
- [x] 키보드
  - keysym 경로: winit 논리 키(Shift·배열 반영, Ctrl 미반영)/문자 → keysym, `UnicodeToKeysym` 테이블(Java 뷰어에서 생성), 키패드는 `KP_*`
  - 한/영·한자 키는 `Hangul`/`Hangul_Hanja` keysym으로 보낸다. QEMU 스캔코드 경로는 M5에서 한다.
  - 눌린 키를 물리 키별로 추적해 같은 keysym으로 놓고, 포커스를 잃으면 키를 모두 해제한다.
  - AltGr 감지(LCtrl 직후 RAlt)는 하지 않는다. Windows에서 AltGr 배열은 Ctrl이 함께 전달된다.
- [x] 비밀번호 입력, CLI(`minivnc-viewer host:display`, `host::port`, `-Password`, `-PasswordFile`, `-Encoding`, `-JPEG`, `-Quality`, `-Subsampling`, `-CompressLevel`, `-Shared`, `-ViewOnly`)
  - SetEncodings 순서는 Java 뷰어(`CMsgWriter.writeSetEncodings`)를 따르되, 디코딩할 수 없는 것은 뺀다.
  - 연결 대화상자(egui)는 M8로 미룬다. egui는 GPU 렌더러가 필요하고, winit 이벤트 루프는 프로세스당 하나뿐이어서 softbuffer 창과 함께 쓰려면 구조가 커진다. 비밀번호 옵션이 없으면 서버가 VNC 인증을 고를 때 콘솔에서 묻는다(`ConnectOptions::ask_password`).
- [x] 요청 파이프라이닝(FBU 시작 시 다음 요청 전송, `Client::read_message_with`)
- **DoD:** Xvnc와 MiniVNC 서버 양쪽에 접속해 일상 작업이 가능하다.
- **검증 기록 (2026-11-02)**
  - 환경: Windows 11의 뷰어 → WSL Ubuntu 26.04의 TurboVNC 3.3.1 Xvnc와 MiniVNC 서버(Xvfb 1280x800 + xfce4). 실제 데스크톱 입력을 주입하지 않으려고, 뷰어 창에만 창 메시지(PostMessage)를 보내 키·마우스를 시험했다.
  - **Xvnc**: 시험 이미지(mixed, 1024x768)를 `-JPEG=0 -CompressLevel=0`으로 받은 창 캡처가 원본과 0픽셀 차이였다. 기본 설정(Tight + JPEG Q95)에서는 최대 채널 오차가 24였다.
  - **MiniVNC 서버**: 터미널 타이핑(소문자, 숫자, 공백, Enter), Ctrl+D, 제목 표시줄 끌기로 창 이동, 휠로 터미널 스크롤백이 모두 동작했다.
  - **스크롤**: 창을 884x561로 줄이자 스크롤바가 생겼고, 끌어서 원격 화면 오른쪽 아래로 이동했다.
  - **연결 종료**: 서버가 끊기면 이유("the server closed the connection")를 출력하고 종료한다.
  - 이 과정에서 서버 버그를 고쳤다: xfce 세션이 끝날 때 XFIXES `GetCursorImage`가 BadAccess를 돌려줘 캡처 스레드가 멈췄다. 이제 커서 없이 계속한다.
  - 사람이 실제 키보드·마우스로 쓰는 감각(한글 IME, Shift 조합, 반응성)은 사용자가 직접 확인한다.

### M5. 고급 프로토콜 기능 (3주)
- [x] **Fence + Continuous Updates + 흐름 제어**
  - `flowcontrol.c`의 Vegas 방식 혼잡 제어를 수치까지 그대로 이식했다(`rfb-server/src/flow.rs`). 업데이트 앞뒤로 RTT ping fence를 보내고, 혼잡하면 예상 시간 뒤에 다시 시도한다(`-noflowcontrol`로 끔).
    - 원본과 다른 점이 하나 있다. 지연 급증으로 창이 4096 아래로 줄어든 직후 Vegas 단계에서 4096을 빼면 원본의 `unsigned`가 최대 창으로 넘어간다. 여기서는 0에서 멈춘 뒤 최소 창으로 맞춘다.
  - SyncNext fence는 다음 메시지를 처리한 뒤 응답하고, 그동안 업데이트를 보내지 않는다(TurboVNC `syncFence`).
  - 뷰어 쪽: 서버 fence 요청에 바로 응답하고, EndOfContinuousUpdates를 받았으면 첫 업데이트 뒤 CU를 켠다(Java `CConn`). CU 중에는 업데이트 요청을 보내지 않는다. 뷰어는 픽셀 포맷을 바꾸지 않으므로 fence payload를 이용한 픽셀 포맷 동기 전환은 하지 않았다.
- [x] **ExtendedDesktopSize / SetDesktopSize**
  - 서버: non-incremental 요청 때와 크기가 바뀔 때 ExtendedDesktopSize(와 DesktopSize)를 보낸다. 요청한 클라이언트에는 reason=client, 다른 클라이언트에는 other-client로 알린다. `-noremoteresize`로 끈다.
  - X11: RandR로 WxH 모드를 만들어 출력에 붙이고 CRTC를 바꾼다. 화면을 두 크기 중 큰 쪽으로 넓힌 상태에서 바꾸므로 실패해도 출력이 꺼지지 않는다. Xvfb는 모드를 바꿀 수 없어 화면 크기만 바꾸며(시작 크기 이하), 크기는 루트 창 ConfigureNotify로 감지한다.
  - X 서버가 허용하는 최대 크기를 넘는 요청은 TurboVNC처럼 거절하지 않고 최대 크기까지만 키운다(사용자 결정). 이미 그 크기면 바로 성공으로 답한다. `InputSink::set_desktop_size`는 실제로 적용한 크기를 돌려준다.
  - 화면은 하나(id 0)로만 다룬다. 다중 화면 배치 요청은 검증만 하고 전체 크기만 반영한다.
  - 뷰어: `-DesktopSize=Auto`(기본)이면 창 크기가 0.5초 동안 그대로일 때 SetDesktopSize를 보낸다. 서버가 요청보다 작게 맞추면 창은 그대로 두고 남는 부분을 여백으로 그린다. `Server`면 창이 원격 크기를 넘지 않는다. 서버가 스스로 크기를 바꾸면 창을 따라 바꾼다.
  - 뷰어 전체 화면(사용자 요청): 최대화 버튼을 누르면 창이 있는 모니터를 테두리 없이 채운다. 원격에서 F11을 써야 하므로 F11 단축키는 두지 않는다. 전체 화면에서는 마우스가 위쪽 가장자리 가운데(버튼 양옆에 230픽셀씩 여백을 둔 약 600픽셀 범위)에 닿으면 뷰어가 직접 그린 연결 바(최소화·복원·닫기)가 나타나고, 바를 벗어나면 사라진다.
- [x] **QEMU Extended Key Event + LED 상태**
  - 서버: `qnum_to_xorgevdev`/`qnum_to_xorgkbd` 표를 TurboVNC 소스(QEMU keymap-gen 생성물)에서 생성했다(`capture-x11/src/qnum_table.rs`). XKB 규칙은 루트의 `_XKB_RULES_NAMES`로 감지하며, evdev 또는 base/xorg가 아니면 확장을 제안하지 않는다. 표에 없는 코드는 keysym 경로로 처리한다(`ExtKeyEvent`).
  - LED: XKB 지시등 "Caps Lock"/"Num Lock"/"Scroll Lock"의 IndicatorStateNotify를 받아 QEMU(1바이트) 또는 VMware(4바이트) LED 의사 사각형으로 보낸다.
  - 뷰어: OS 스캔코드 대신 winit의 물리 키(`KeyCode`) → qnum 표를 써서 Windows와 Linux가 같은 코드를 쓴다(E0 확장 키는 0x80).
  - **한/영 키 → 0x72 + `XK_Hangul`, 한자 키 → 0x71 + `XK_Hangul_Hanja`를 기본 내장했다.**
    - Windows 한국어 배열에서 winit이 주는 논리 키 `HangulMode`/`HanjaMode`로 감지하므로 raw input은 필요 없다. 물리 위치(오른쪽 Alt/Ctrl)와 상관없이 위 코드를 보낸다.
    - 서버가 QEMU 확장을 지원하지 않으면 keysym(`Hangul`/`Hangul_Hanja`)만 보낸다.
  - AltGr: QEMU 경로에서는 오른쪽 Alt의 키코드가 그대로 가서 서버 키맵이 Level3를 처리한다. keysym 경로의 Windows AltGr(가짜 LCtrl) 보정은 하지 않았다.
  - LED 동기화(사용자 결정): 서버 LED 상태를 받거나 창이 포커스를 얻을 때, 로컬 락 상태와 다른 락 키만 서버에 눌러 맞춘다. 로컬 키보드는 건드리지 않는다. 로컬 락 상태는 Windows에서만 읽는다(winit이 알려 주지 않음).
- [x] **커서 모양**
  - 서버: XFixes ARGB 커서를 RichCursor(채널 상위 비트, 알파 > 127이 마스크) 또는 XCursor로 보낸다. ARGB 커서에는 두 색이 없으므로 XCursor는 어두운 픽셀을 검정 전경, 나머지를 흰 배경으로 만든다.
  - 다른 클라이언트의 포인터 이동은 PointerPos로 알린다(자기 이동은 알리지 않음).
  - 커서 모양을 받지 않는 클라이언트를 위해 서버가 커서를 그린 프레임버퍼를 따로 유지한다(커서 합성을 캡처에서 `rfb-server`로 옮김). TurboVNC처럼 화면 하나에 커서를 넣고 빼는 대신 버퍼를 둘 둔다.
  - 뷰어: 받은 모양을 winit 사용자 커서로 쓰고, 빈 커서면 로컬 커서를 숨긴다. 모양을 받기 전에는 점 커서를 쓴다.
- [x] **Extended Clipboard**(UTF-8, zlib) 양방향, 레거시 cut text 폴백, `-maxclipboard`, `-noclipboardsend`/`-noclipboardrecv`, `-noprimarysync`
  - 서버(X11): 클립보드 전용 연결·스레드가 XFixes로 CLIPBOARD(와 PRIMARY) 소유자 변경을 감시해 UTF8_STRING(없으면 STRING)으로 가져온다. 클라이언트 텍스트는 선택을 소유해 TARGETS/UTF8_STRING/TEXT/STRING 요청에 답한다. INCR 전송은 지원하지 않는다.
  - 클라이언트가 Notify를 보내면 TurboVNC는 X 앱이 요청할 때 가져오지만, MiniVNC는 바로 Request를 보낸다.
  - 뷰어: 서버 Caps에 크기 1 MiB로 답해 서버가 바로 보내게 하고, 창이 포커스를 얻을 때 로컬 클립보드가 바뀌었으면 Notify(확장) 또는 ClientCutText(레거시)로 알린다(`arboard`).
- [x] ExtendedMouseButtons(뷰어: 뒤로/앞으로 = 버튼 8/9, 서버: 15개까지 XTest로 전달), Bell(서버: XKB BellNotify, 뷰어: Windows `MessageBeep`)
- **DoD:** TurboVNC 뷰어 ↔ MiniVNC 서버, Xvnc ↔ MiniVNC 뷰어 양방향에서 CU/Fence가 활성화되고, 한/영·한자 키가 서버 `xev`에서 keycode 130/131로 나온다.
- **검증 기록 (2026-11-03)**
  - 환경: Windows 11 ↔ WSL Ubuntu 26.04(TurboVNC 3.3.1 Xvnc, MiniVNC 서버 + Xvfb 1280x800 + xfce4, XKB 규칙 evdev/kr). MiniVNC 뷰어 입력은 뷰어 창에만 창 메시지를 보내 시험했다.
  - **CU/Fence**: TurboVNC Java 뷰어 → MiniVNC 서버에서 CU, QEMU 키, LED 상태, 확장 마우스 버튼이 켜졌다(뷰어 로그). MiniVNC 뷰어 → Xvnc와 → MiniVNC 서버에서도 CU와 QEMU 키 코드가 켜졌다.
  - **한/영·한자**: MiniVNC 뷰어에서 `VK_HANGUL`/`VK_HANJA`를 보내자 Xvnc와 MiniVNC 서버 양쪽의 `xev`에 keycode 130 `Hangul`, 131 `Hangul_Hanja`가 나왔다.
  - **원격 크기**: MiniVNC 뷰어 창을 줄이자 Xvfb 화면이 884x561로 바뀌었고, xfce가 새 크기에 맞춰 다시 배치됐다.
  - **클립보드**: Java 뷰어의 클립보드(Notify → Request → Provide)가 X CLIPBOARD로 들어가 터미널 붙여넣기로 확인됐다. 서버 → 클라이언트 방향과 레거시/확장 형식은 루프백 테스트로 확인했다. MiniVNC 뷰어의 로컬 클립보드 연동은 사용자 PC 클립보드를 바꾸므로 자동 시험하지 않았다.
  - **회귀**: 무손실 Xvnc 화면은 원본과 0픽셀 차이였고, mpv 30fps 영상의 업데이트 속도는 23.4/s로 M3와 같았다.
  - 루프백 테스트를 추가했다: fence와 CU, 커서 모양과 위치, 데스크톱 크기 변경(성공/거부), QEMU 키·LED·확장 마우스 버튼, 클립보드 양방향과 벨.

### M6. 보안 (2주)
- [x] **Tight 보안 타입**(16): 터널/인증 능력, 상호작용 능력(서버 송신, 클라이언트 수신 후 무시)
  - 서버는 TurboVNC처럼 Tight를 항상 제안하고, None은 다른 타입이 없을 때만 제안한다. 뷰어는 Java 뷰어처럼 Tight가 있으면 Tight를 고른 뒤 서버의 첫 사용 가능한 인증 능력을 쓴다.
  - 상호작용 능력에는 MiniVNC가 실제로 보내는 인코딩만 넣는다(TurboVNC는 Hextile 등도 넣는다). Java 뷰어는 Tight를 고르면 이 목록을 반드시 읽으므로, 빠지면 ServerInit 뒤에 멈춘다.
  - 보안 타입 표(`rfb-proto/src/security.rs`)는 TurboVNC `auth.c`의 순서와 기본값을 따르며 서버와 뷰어가 함께 쓴다.
- [x] **VeNCrypt** 0.2와 서브타입 협상 (사용자 결정: 서버와 뷰어 모두 OpenSSL)
  - X509None/X509Vnc/X509Plain: `-x509cert`/`-x509key`(PEM). 인증서와 키는 서버 시작 때 읽으므로, 잘못되면 TurboVNC와 달리 접속 때가 아니라 시작 때 실패한다. 인증서가 없으면 기본 목록에서 X509 타입을 뺀다.
  - TLSNone/TLSVnc/TLSPlain: 익명 DH. TLS 1.3에는 익명 암호군이 없으므로 TLS 1.2까지만 쓰고, OpenSSL 보안 수준 0으로 `aNULL`을 허용한다. TurboVNC는 접속마다 DSA로 DH 파라미터를 만들지만, MiniVNC는 고정 2048비트 그룹(RFC 5114)을 쓴다.
  - `permitted-cipher-suites`는 OpenSSL 암호군 목록으로 그대로 넘긴다. `tls-key-length`는 지원하지 않는다.
  - 뷰어: TLS로 바뀐 뒤에도 읽기 스레드와 쓰기(창)가 한 연결을 나눠 써야 하므로 `rfb_client::Stream`을 만들었다. 읽을 데이터가 올 때까지 잠금 없이 기다린 뒤 TLS 레코드를 읽어, 쓰기가 읽기를 기다리지 않는다.
  - 뷰어 인증서 검사(Java `CSecurityTLS` 순서): 유효 기간(만료면 확인), `~/.vnc/x509_savedcerts.pem`에 저장된 인증서면 통과, 호스트 이름(SAN의 DNS/IP, 없으면 CN; 다르면 확인), 시스템 CA와 `-X509CA`(기본 `~/.vnc/x509_ca.pem`). 모르는 발급자면 인증서 정보와 SHA-256 지문을 보여 주고 콘솔에서 묻고, 수락하면 저장한다(Java 뷰어와 같은 파일 형식).
- [x] Plain/UnixLogin: PAM 인증(Linux), 세션 소유자 검사. UTF-8 길이 버그는 재현하지 않는다.
  - PAM 서비스 이름은 `minivnc`(사용자 결정, 예제 `scripts/pam.d/minivnc`), `pam-service-name`으로 바꾼다. 실패 메시지는 TurboVNC `authpam.c`와 같다.
  - `pam` 크레이트는 `PAM_RHOST`를 설정할 수 없어 PAM 모듈이 클라이언트 주소를 모른다. PAM 세션(`-pamsession`)은 지원하지 않는다.
  - `enable-user-acl`이면 ACL은 세션 소유자(전체 제어)로 시작한다. TurboVNC처럼 `vncpasswd -a`로 사용자를 더하는 기능은 없다.
- [x] view-only 비밀번호(16바이트 파일), OTP, 무차별 대입 차단(5회 실패 / 10초, 이후 2배)
  - OTP(사용자 결정): `minivnc-passwd -o [-v] [-display :N]`가 8자리 숫자를 만들어 루트 창 속성 `VNC_OTP`에 쓰면, 서버가 읽고 속성을 지운다(TurboVNC 방식). `-c`로 지운다. 한 번 성공하면 없어진다.
  - 차단은 TurboVNC처럼 VNC 비밀번호 실패만 센다. 차단 중인 호스트는 인증 전에 ConnFailed로 거절한다.
- [x] 보안 설정 파일(`/etc/minivncserver-security.conf`, 사용자 결정. TurboVNC 문법 호환 파서, 소유자/권한 검사)
  - root나 서버 사용자 소유이고 그룹/기타 쓰기 권한이 없어야 한다. 파일이 없으면 기본값을 쓴다. 지원하지 않는 항목은 경고만 한다.
- **DoD:** TurboVNC 뷰어의 기본 SecurityTypes 목록 순서대로 각 타입에 접속할 수 있다. 실패·차단 동작이 원본과 같다.
- **검증 기록 (2026-11-03)**
  - 환경: Windows 11의 TurboVNC 3.3.1 Java 뷰어와 MiniVNC 뷰어 → WSL Ubuntu 26.04의 MiniVNC 서버(`-source pattern`, 자체 서명 인증서 CN=localhost).
  - **Java 뷰어, 기본 SecurityTypes**: 서버 기본 목록(tlsvnc, tlsotp, tlsplain, x509vnc, x509otp, x509plain, vnc, otp, unixlogin, plain)에서 Tight → VeNCrypt → TLSVnc를 골랐고, TLS 1.2 `ADH-AES256-GCM-SHA384`로 접속해 Continuous Updates까지 켜졌다.
  - **Java 뷰어, 타입별**(`-securitytypes`로 하나씩): X509Vnc·X509None(TLS 1.3, `-X509CA`로 신뢰), TLSVnc·TLSNone(익명 DH), VNC, None은 접속했다. OTP는 설정 전 "The one-time password has not been set on the server"로 거절됐다. TLSPlain·X509Plain·UnixLogin은 틀린 비밀번호에 PAM이 AUTH_ERR를 돌려 "Authentication failed", 소유자가 아닌 사용자는 "User denied access"를 받았다.
  - **차단**: 틀린 비밀번호로 4번은 "Authentication failed", 5번째는 "Authentication failed.  Client temporarily blocked", 그다음에는 맞는 비밀번호도 "Too many authentication failures.  Client temporarily blocked"로 거절됐다.
  - **MiniVNC 뷰어 → MiniVNC 서버**: 기본 목록으로 TLSVnc 접속, X509Vnc는 모르는 발급자 확인에서 거절하면 "X.509 certificate not trusted", 수락하면 접속하고 저장한 뒤 다음 접속은 묻지 않았다. TLS 위에서 테스트 패턴이 계속 갱신됐다.
  - **MiniVNC 뷰어 → TurboVNC 3.3.1 Xvnc**: 기본 목록(TLSVnc, `ADH-AES256-GCM-SHA384`), X509Vnc·X509None(TLS 1.3), VNC로 접속해 CU까지 켜졌다. 이 시험에서 뷰어가 Tight 상호작용 능력을 읽지 않아 ServerInit 다음 메시지를 잘못 해석하는 문제를 찾아 고쳤다(Java 뷰어 → MiniVNC 서버의 멈춤과 같은 원인).
  - **OTP**(앞선 시험): WSL :7에서 설정 전 거절, 발급한 비밀번호로 한 번 성공, 재사용 거절.
  - 루프백 테스트를 추가했다: Tight/VeNCrypt 로그인과 ACL, 차단, OTP, 익명 TLS와 X.509(거절, 수락 후 저장, 호스트 이름 불일치).
  - **검증하지 못한 것**: 올바른 비밀번호로 PAM 로그인 성공. 사용자 비밀번호가 필요하고, `/etc/pam.d/minivnc` 설치에는 root가 필요하다.

### M7. 성능 기능 (3주)
- [x] **멀티스레드 Tight**
  - 수평 띠 분할, `min(nthreads, area/MAXRECTSIZE)`(`-nthreads`, `-nomt`, 기본은 CPU 수, 최대 4)
  - 스레드별 zlib 스트림 분배를 원본과 같은 결정적 규칙으로 한다.
  - 출력은 순서대로 기록한다.
  - `rayon` scoped pool을 쓴다.
  - 원본과 다른 점: LastRect를 받는 클라이언트에만 띠로 나눈다. 받지 않으면 미리 알린 사각형 수와 띠가 만든 수가 다를 수 있기 때문이다.
- [x] **Interframe Comparison:** 256 블록 비교, `-interframe`/`-nointerframe`
  - 자동 모드는 TurboVNC 소스대로 CL 5 이상(9 포함)에서 켜진다. 압축 레벨이 든 SetEncodings를 받을 때만 바뀐다.
  - 바뀐 블록이 없으면 1x1 사각형을 보낸다.
- [x] **ALR:** JPEG 영역 기록, 타이머, `-alr`/`-alrqual`/`-alrsamp`, 혼잡 제어 우회
  - XDamage는 그리기 종류를 알려 주지 않는다. 그래서 TurboVNC의 "PutImage 영역만" 구분 없이, JPEG로 보낸 영역 전체를 다시 보낸다(TurboVNC `TVNC_ALRALL`과 같다).
  - 서버가 CopyRect를 만들지 않으므로 CopyRect의 lossy 전파는 해당이 없다.
- [x] 뷰어: "Lossless Refresh" 단축키(Ctrl+Alt+Shift+L). Java 뷰어처럼 무손실 SetEncodings, 전체 요청, 원래 SetEncodings를 차례로 보낸다.
- [x] `rfb-bench`: 실시간 측정(업데이트/s·Mpixel/s·Mbit/s), `-record`/`-replay`로 녹화와 디코딩 벤치마크. 녹화 도구를 따로 두지 않고 하나로 합쳤다. 서버 `-profile`은 TurboVNC `TVNC_PROFILE`과 같은 형식에 구간 시간을 더했다.
- [x] 최적화(측정한 병목만, 사용자 결정)
  - Linux JPEG를 libjpeg-turbo 3 정적 빌드로 바꿨다(사용자 결정).
  - JPEG 입력과 출력을 프레임버퍼 그 자리에서 쓰고, 핸들을 재사용한다.
  - 팔레트 색인을 고정 크기 표로 바꿨고, 색은 서버 픽셀로 센다.
  - X 이미지 읽기와 포인터 조회의 왕복을 묶었다.
  - 수신 버퍼에 소비 위치를 두었고, 뷰어는 부분 표시를 한다.
  - 정적 분석 제안의 검토와 적용 결과는 `docs/improvement-plan.md`에 있다. SIMD 팔레트 탐색과 버퍼 풀은 병목이 아니어서 하지 않았다.
- **DoD:** §3.1의 성능 목표를 달성한다(벤치마크 결과를 `docs/bench.md`에 기록).
- **검증 기록 (2026-11-04)**: 상세는 `docs/bench.md`에 있다.
  - TurboVNC 3.3.1 Xvnc와 같은 부하(mpv 영상 2종, x11perf 텍스트)와 5개 프리셋에서 비교했다. 인코딩 처리량은 81~138%, 대역폭은 영상에서 0.96~1.01배, 텍스트에서 Mpixel당 1.01배였다. 두 기준을 만족한다.
  - 남은 차이는 외부 캡처 구조에서 오는 CPU다(영상에서 Xvfb 포함 15~20% 대 Xvnc 8~14%).
  - 녹화 재생 디코딩은 JPEG 4.15 → 2.60 ms/update, 무손실 1.68 → 1.51 ms/update로 줄었다.
  - 루프백 테스트를 추가했다: 띠 분할 다중 프레임 왕복, JPEG 영역 보고, Interframe(설정·자동·CL), ALR 무손실 복원.

### M8. 호환성·편의 기능 (3주, P2)
- [x] ZRLE, Hextile 인코더/디코더(noVNC, TigerVNC, RealVNC 호환)
  - TurboVNC `hextile.c`, `zrleencodetemplate.c`를 옮겼다. ZRLE는 64x64 타일, zlib 레벨 6, 사각형마다 sync flush, 팔레트 최대 127색, CPIXEL(32비트 픽셀의 색이 위나 아래 3바이트에 들어가면 3바이트)이다.
  - 디코더는 압축 해제 크기와 타일 범위를 제한한다.
- [x] WebSocket(같은 포트 자동 감지, RFC 6455, binary/base64, wss)
  - 연결 뒤 100 ms 동안 첫 바이트를 본다. TLS 핸드셰이크(0x16/0x80)면 wss, `GET `이면 ws, 아무것도 오지 않으면 RFB다.
  - wss에는 X509 인증서(`-x509cert`)가 필요하다. 하위 프로토콜 `binary`가 없고 `base64`만 있으면 base64로 보낸다.
  - **noVNC로 접속은 검증하지 못했다**(브라우저 수동 시험 필요).
- [x] 역방향 연결(서버 `-connect host[:5500][#ID]`, 뷰어 `-listen [PORT]`), repeater `ID:n`(250바이트)
  - 역방향 연결에는 X509 보안 타입을 내지 않는다(뷰어가 서버 인증서를 받을 수 없다). 공유 정책으로 다른 연결을 끊지 않는다. 보안 설정 `no-reverse-connections`를 따른다.
- [x] 뷰어
  - 스케일링(`-Scale` 퍼센트 10~400/Auto/FixedRatio, 최근접 픽셀). 100이 아니면 DesktopSize는 Server다.
  - 전체 화면과 다중 모니터 span(`-Span` Primary/All/Auto). winit에는 여러 모니터 전체 화면이 없어서, 모니터를 모두 덮는 테두리 없는 창을 쓴다.
  - 키보드 그랩(`-GrabKeyboard` FS/Always/Manual, Ctrl+Alt+Shift+G). Windows `WH_KEYBOARD_LL`을 쓴다. 원본의 `MakeLParam` 버그는 고쳤다. Linux는 아직 그랩하지 않는다.
  - SSH 터널(`-via`, 외부 ssh `-N -L`, `MINIVNC_SSH`). 로컬 포트가 열리면 접속한다.
  - 연결 정보 파일(TurboVNC `.turbovnc`, `-config` 또는 위치 인자), 기본값 `~/.vnc/default.turbovnc`, 옵션 저장(`-SaveConfig`, 비밀번호 제외)
  - 연결 대화상자(egui) GUI는 다시 미뤘다(사용자 결정).
- [x] `minivnc-session`(Rust 바이너리, 사용자 결정)
  - Xvfb와 서버 실행, 디스플레이 번호 할당(TurboVNC `CheckDisplayNumber`), xauth 쿠키, xstartup(`~/.vnc/xstartup.minivnc`, 없으면 만든다)
  - `-kill`/`-list`, OTP 발급(`-otp`), `-fg`, `-noautokill`, `-rfbauth`, `-- 서버 옵션`
  - 감독 프로세스가 서버, Xvfb, 앱 중 하나가 끝나거나 신호를 받으면 세션 전체를 정리한다(pid 파일, xauth 항목, 앱 프로세스 그룹).
  - `scripts/start-minivnc-server.sh`는 `minivnc-session -fg`의 래퍼가 됐다.
- [ ] 스크롤 감지 기반 CopyRect(서버, 실험적) — 이번 범위에서 뺐다(사용자 결정).
- [ ] Windows 서버 백엔드(DXGI Desktop Duplication + SendInput) — 선택. 이번 범위에서 뺐다(사용자 결정).
- **검증 기록 (2026-11-04)**
  - TurboVNC Xvnc와 MiniVNC 서버에서 ZRLE와 Hextile 스냅숏은 Raw와 픽셀이 같다. Hextile 바이트 수는 TurboVNC와 같고, ZRLE는 0.2~2.4% 크다. TurboVNC Java 뷰어가 MiniVNC 서버의 ZRLE와 Hextile을 그린다.
  - 루프백 테스트를 추가했다: 전체 업데이트 인코딩 목록에 ZRLE/Hextile, 같은 포트의 WebSocket(binary, base64, wss), 역방향 연결과 repeater ID.
  - 뷰어 실행 확인(Windows): 1280x720 원격 화면에서 `-Scale=50` 창은 640x360, `-Scale=150` 창은 1920x1080이다.
  - `minivnc-session`(WSL): 빈 번호 할당, 접속(VNC 인증), 서버 옵션 전달, 앱 종료 시 자동 종료, `-kill`/`-kill :*`, `-fg`에서 Ctrl+C, 비밀번호 파일이 없을 때 오류를 확인했다. 남은 Xvfb, 앱, pid 파일은 없었다.
  - **검증하지 못한 것**: 브라우저 noVNC 접속, 다중 모니터 span(모니터 하나), 키보드 그랩의 실제 키 가로채기(데스크톱 입력 주입을 하지 않는다), 실제 SSH 게이트웨이를 거친 `-via`, 실제 뷰어의 `-listen`(WSL NAT에서 Windows로 들어오려면 방화벽 규칙이 필요하다), xfce4 xstartup 세션.

### 일정 요약

| 마일스톤         | 기간 | 누적 |
| ---------------- | ---- | ---- |
| M0 기반          | 1주  | 1주  |
| M1 프로토콜 코어 | 2주  | 3주  |
| M2 Tight         | 3주  | 6주  |
| M3 X11 서버      | 2주  | 8주  |
| M4 뷰어 MVP      | 3주  | 11주 |
| M5 고급 프로토콜 | 3주  | 14주 |
| M6 보안          | 2주  | 16주 |
| M7 성능          | 3주  | 19주 |
| M8 호환성·편의   | 3주  | 22주 |

M3와 M4는 서로 독립적이어서 병렬로 진행할 수 있다(1인 기준 22주, 2인이면 약 14주).

---

## 6. 테스트 전략

| 종류             | 내용                                                                                                                                                                                                               |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 단위 테스트      | 메시지 파싱/직렬화 왕복, compact length, DES(알려진 벡터), 비밀번호 파일, 픽셀 변환, 영역 연산(proptest)                                                                                                           |
| 코덱 골든 테스트 | 원본 Xvnc `-capture`로 녹화한 스트림을 MiniVNC 디코더로 재생하고 원본 화면과 비교. 인코더 출력은 자체 디코더와 (가능하면) TurboVNC 디코더로 왕복 검증                                                              |
| Fuzz             | `cargo-fuzz`: 서버 메시지 파서, 클라이언트 메시지 파서, Tight/ZRLE/Hextile 디코더, 보안 핸드셰이크                                                                                                                 |
| 상호 운용        | `tests/interop/`: Docker(Ubuntu + TurboVNC 3.3.2 + Xvfb)에서 자동화. 조합: {MiniVNC, TurboVNC} 서버 × {MiniVNC, TurboVNC Java, TigerVNC, noVNC} 뷰어 × {프리셋 5종} × {보안 타입}. 헤드리스 뷰어로 스크린샷을 비교 |
| 성능             | `rfb-bench`: 표준 워크로드(2D 앱, 동영상, 3D 녹화)에서 인코딩 Mpixel/s, 바이트 수, 디코딩 시간을 원본과 비교                                                                                                       |
| 수동 검증        | 키보드(한글 레이아웃, AltGr 레이아웃, 한/영·한자 키 `xev` 확인), 클립보드 한글, 다중 모니터                                                                                                                        |

---

## 7. 위험 요소와 대응

| 위험                                                                | 영향                     | 대응                                                                                                                                             |
| ------------------------------------------------------------------- | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| 캡처형 서버는 Xvnc보다 손상 정보가 부정확하고 CopyRect를 알 수 없다 | 대역폭과 CPU 사용 증가   | interframe을 기본으로 켜고, XDamage 영역을 병합하고, 스크롤 감지로 보완한다. 필요하면 장기적으로 Xvfb 기반 "xf86 드라이버 모듈" 방식을 검토한다. |
| zlib 출력 비호환(백엔드 차이)                                       | Tight 디코딩 실패        | 표준 zlib 스트림 형식만 쓴다(deflate 출력은 달라도 inflate 호환). 상호 운용 테스트로 확인한다.                                                   |
| rustls에 익명 DH가 없다                                             | TLSNone/TLSVnc 미지원    | 모든 TLS를 OpenSSL로 처리한다(M6). Windows는 소스에서 빌드하므로 Strawberry Perl이 필요하다.                                                      |
| 한글 IME와 키 입력 충돌(winit IME)                                  | 키가 누락되거나 중복된다 | 원격 세션 중에는 로컬 IME를 끄고, raw 스캔코드 경로를 쓴다. 한/영 키는 로컬에서 소비하지 않고 원격으로 보낸다.                                   |
| libjpeg-turbo 빌드 의존성(Windows)                                  | 빌드가 복잡해진다        | `turbojpeg` 크레이트의 vendored 빌드를 쓰고, 순수 Rust JPEG 백엔드를 폴백으로 둔다.                                                              |
| X 키보드 매핑의 복잡성                                              | 특수 레이아웃에서 오입력 | `input-xkb.c` 로직을 그대로 이식하고, QEMU 키 경로를 기본으로 한다.                                                                              |
| 범위 확장(feature creep)                                            | 일정 지연                | §3.2 우선순위를 지키고, P2는 MVP 이후에 착수한다.                                                                                                |

---

## 8. 라이선스

TurboVNC는 **GPL v2**다. MiniVNC는 TurboVNC 소스(상수 표, 알고리즘, 키 매핑 테이블)를 직접 이식한다. 따라서 파생 저작물로 보고 **GPL-2.0-or-later**로 배포하는 것이 안전하다. 키맵 테이블은 QEMU keymap-gen(GPL-2.0+)에서 생성한다. 원본 저작권 고지는 `NOTICE` 또는 `LICENSE`에 함께 적는다.

---

## 9. 참고 원본 위치 (구현 중 대조용)

| 주제             | 원본 위치                                                                                                                                   |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| 프로토콜 상수    | `turbovnc/common/rfb/rfbproto.h`, `turbovnc/java/com/turbovnc/rfb/RFB.java`                                                                 |
| 서버 메시지 처리 | `turbovnc/unix/Xvnc/programs/Xserver/hw/vnc/rfbserver.c` (`rfbProcessClientNormalMessage`, `rfbSendFramebufferUpdate`)                      |
| Tight 인코더     | `.../hw/vnc/tight.c` (`SendRectEncodingTight`, `SendSubrect`, `PaletteInsert`, `CompressData`, `SendJpegRect`)                              |
| Tight 디코더     | `turbovnc/java/com/turbovnc/rfb/TightDecoder.java`                                                                                          |
| 흐름 제어        | `.../hw/vnc/flowcontrol.c` (`HandleRTTPong`, `UpdateCongestion`, `rfbIsCongested`)                                                          |
| 보안             | `.../hw/vnc/auth.c`, `authpam.c`, `turbovnc/unix/libvncauth/vncauth.c`, `java/.../rfb/CSecurity*.java`                                      |
| 키보드(서버)     | `.../hw/vnc/kbdptr.c`, `input-xkb.c`, `qnum_to_xorgevdev.c`                                                                                 |
| 키보드(뷰어)     | `turbovnc/java/com/turbovnc/vncviewer/CConn.java` (`getRFBKeyCode`, `writeKeyEvent`), `rfb/Keycodes.java`, `win/vncviewer/LowLevelHook.cpp` |
| 클립보드         | `.../hw/vnc/cutpaste.c`, `java/.../rfb/CConnection.java`                                                                                    |
| 데스크톱 크기    | `.../hw/vnc/randr.c`, `CConn.java` (`computeScreenLayout`, `resizeFramebuffer`)                                                             |
| 세션 스크립트    | `turbovnc/unix/vncserver.in`, `turbovnc/unix/turbovncserver*.conf`                                                                          |
| 성능 문서        | `turbovnc/doc/performance.txt`                                                                                                              |
