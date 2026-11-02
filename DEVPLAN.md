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
| zlib              | `flate2` (`zlib-rs` 또는 `zlib` 백엔드)                                | 영구 `Compress`, `FlushCompress::Sync`, `set_level`이 필요하다. **zlib 호환 출력이 필수다.**                            |
| JPEG              | `turbojpeg` 크레이트 (libjpeg-turbo)                                   | 4:4:4/4:2:2/4:2:0/Gray를 지원하고 성능이 TurboVNC와 같다. 대안은 순수 Rust 백엔드(feature)다.                           |
| DES               | `des` 크레이트                                                         | VNC 키 비트 반전을 직접 처리해야 한다.                                                                                  |
| TLS               | `rustls` (X509*) + `openssl` feature (TLSNone/TLSVnc/TLSPlain 익명 DH) | rustls는 익명 DH(aNULL)를 지원하지 않는다. 따라서 익명 TLS는 openssl feature로만 제공한다.                              |
| X11               | `x11rb` (shm, damage, xfixes, xtest, randr, xkb)                       | 순수 Rust이고 확장을 모두 지원한다.                                                                                     |
| 창/그리기         | `winit` + `softbuffer` (필요 시 `wgpu`로 스케일링)                     | 가볍고 크로스 플랫폼이다.                                                                                               |
| 클립보드          | `arboard`                                                              | Windows/X11/macOS를 지원한다.                                                                                           |
| GUI 대화상자      | `egui` (옵션/접속/비밀번호)                                            | 즉시 모드 GUI라 간단하다.                                                                                               |
| SSH               | 1단계는 외부 `ssh` 실행, 2단계는 `russh`                               | JSch와 같은 기능 범위                                                                                                   |
| 로깅              | `tracing`                                                              | `-loglevel`에 대응한다.                                                                                                 |
| CLI               | `clap` + TurboVNC 스타일(`-Name=value`) 호환 파서                      | 기존 스크립트와 호환된다.                                                                                               |
| PAM               | `pam` 크레이트 (feature)                                               | Linux 전용                                                                                                              |

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
- [ ] **디코더** (먼저 만들어 인코더 검증에 쓴다)
  - Fill/JPEG/basic
  - copy, palette(1bpp/8bpp), gradient 필터
  - 영구 zlib 스트림 4개와 리셋 비트, NoZlib, TPIXEL
- [ ] **인코더**
  - `tightConf`와 CL 매핑
  - 단색 영역 분할(`CheckSolidTile`/`FindBestSolidArea`/`ExtendSolidArea`)
  - `SendRectSimple` 분할
  - `FillPalette`(빈도 정렬 해시 팔레트)
  - Mono/Indexed/FullColor/JPEG 분기
  - TPIXEL `Pack24`
  - zlib 스트림 round-robin
  - LastRect(0xFFFF)
- [ ] JPEG 백엔드: `turbojpeg`, 서브샘플링 매핑 {444, 420, 422, GRAY}, 회색조 Fill 변환
- [ ] 품질 의사 인코딩 처리: coarse/fine/subsamp, 순서에 따라 덮어쓰기
- [ ] CopyRect 인코딩/디코딩(와이어 형식)
- **DoD**
  - Java 뷰어 5개 프리셋 전부에서 화면이 깨지지 않는다.
  - Xvnc → MiniVNC 클라이언트에서 디코딩 결과가 무손실 모드에서 서버 화면과 비트 단위로 같다.
  - 같은 입력에서 TurboVNC 대비 출력 크기 ±10% 이내(녹화 데이터 기준).

### M3. 실제 화면 서버 — X11 (2주)
- [ ] `capture-x11`
  - XShm 캡처, XDamage 손상 영역, RandR 지오메트리
  - XFixes 커서
  - XTest 입력과 keysym→keycode 변환(`input-xkb.c` 로직: Shift/Level3 보정, 동적 keysym 할당, 눌린 키 추적)
- [ ] **업데이트 스케줄러**
  - modified/requested 영역 관리, 지연 업데이트(기본 1~40ms 설정)
  - 100개 초과 사각형 병합
  - non-incremental 요청 처리
- [ ] 다중 클라이언트: shared/exclusive 정책(`-alwaysshared`, `-nevershared`, `-disconnect`), 포인터 잠금
- [ ] `minivnc-server` CLI: `-rfbport`, `-rfbauth`, `-securitytypes`, `-deferupdate`, `-localhost`, `-viewonly`, `-display`
- **DoD:** Xvfb + xfce4 세션을 Java 뷰어로 일상적으로 쓸 수 있다(타이핑, 창 이동, 동영상 재생).

### M4. MiniVNC 뷰어 MVP (3주)
- [ ] `winit` + `softbuffer` 창, 손상 영역 블릿, 창 크기 = 원격 크기, 스크롤
- [ ] 마우스: 버튼, 휠(4/5/6/7), 좌표 변환
- [ ] 키보드
  - keysym 경로: 논리 키/문자 → keysym, `UnicodeToKeysym` 테이블
  - 눌린 키 추적, 포커스를 잃으면 키를 모두 해제
- [ ] 연결 대화상자(egui), 비밀번호 입력, CLI(`minivnc-viewer host:display`, `-Password`, `-Encoding`, `-Quality`, ...)
- [ ] 요청 파이프라이닝(FBU 시작 시 다음 요청 전송)
- **DoD:** Xvnc와 MiniVNC 서버 양쪽에 접속해 일상 작업이 가능하다.

### M5. 고급 프로토콜 기능 (3주)
- [ ] **Fence + Continuous Updates + 흐름 제어**
  - `flowcontrol.c`의 Vegas 방식 혼잡 제어를 수치까지 그대로 이식한다.
  - 뷰어 쪽: SyncNext fence → CU 활성화, fence payload를 이용한 픽셀 포맷 동기 전환
- [ ] **ExtendedDesktopSize / SetDesktopSize**
  - 서버: RandR로 크기 변경, `no-remote-resize`, 첫 FBU에 항상 포함
  - 뷰어: `DesktopSize=Auto`일 때 창 크기에 맞춰 원격 크기를 바꾼다.
- [ ] **QEMU Extended Key Event + LED 상태**
  - 서버: `qnum_to_xorgevdev`/`qnum_to_xorgkbd` 테이블(QEMU keymap-gen 데이터에서 생성), XKB 룰(evdev/base) 감지
  - 뷰어: Windows 스캔코드(확장 키 0x80 처리, RShift 보정), X11 keycode → qnum
  - **한/영 키 → 0x72 + `XK_Hangul`, 한자 키 → 0x71 + `XK_Hangul_Hanja`를 기본 내장한다.**
    - Windows: `VK_HANGUL`/`VK_HANJA`를 raw input으로 감지한다.
    - keysym 경로로 폴백할 때도 keysym을 보낸다(원본 패치는 QEMU 경로가 없으면 키를 버린다).
  - AltGr 보정, LED 동기화
- [ ] **커서 모양:** RichCursor/XCursor/PointerPos 송신(서버)과 로컬 커서 적용(뷰어)
- [ ] **Extended Clipboard**(UTF-8, zlib) 양방향, 레거시 cut text 폴백, `MaxClipboard`
- [ ] ExtendedMouseButtons, Bell
- **DoD:** TurboVNC 뷰어 ↔ MiniVNC 서버, Xvnc ↔ MiniVNC 뷰어 양방향에서 CU/Fence가 활성화되고, 한/영·한자 키가 서버 `xev`에서 keycode 130/131로 나온다.

### M6. 보안 (2주)
- [ ] **Tight 보안 타입**(16): 터널/인증 능력, 상호작용 능력(서버 송신, 클라이언트 수신 후 무시)
- [ ] **VeNCrypt** 0.2와 서브타입 협상
  - X509None/X509Vnc/X509Plain (`rustls`, `-x509cert`/`-x509key`)
  - TLSNone/TLSVnc/TLSPlain (`openssl` feature, 익명 DH)
  - 뷰어: CA 검증, 미확인 인증서 지문 저장 및 확인 대화상자
- [ ] Plain/UnixLogin: PAM 인증(Linux, feature), 세션 소유자 검사. UTF-8 길이 버그는 재현하지 않는다.
- [ ] view-only 비밀번호(16바이트 파일), OTP, 무차별 대입 차단(5회 실패 / 10초, 이후 2배)
- [ ] 보안 설정 파일(`/etc/turbovncserver-security.conf` 문법 호환 파서, 소유자/권한 검사)
- **DoD:** TurboVNC 뷰어의 기본 SecurityTypes 목록 순서대로 각 타입에 접속할 수 있다. 실패·차단 동작이 원본과 같다.

### M7. 성능 기능 (3주)
- [ ] **멀티스레드 Tight**
  - 수평 띠 분할, `min(nthreads, area/MAXRECTSIZE)`
  - 스레드별 zlib 스트림 분배를 원본과 같은 결정적 규칙으로 한다.
  - 출력은 순서대로 기록한다.
  - `rayon` scoped pool을 쓴다.
- [ ] **Interframe Comparison:** 256 블록 비교, CL 5~8 자동 활성화, `-interframe`/`-nointerframe`
- [ ] **ALR:** lossy/lossless 영역 기록, CopyRect가 lossy를 전파하도록 처리, 타이머, `-alr`/`-alrqual`/`-alrsamp`, 혼잡 제어 우회
- [ ] 뷰어: "Lossless Refresh" 단축키(Ctrl+Alt+Shift+L)
- [ ] `rfb-record`/`rfb-bench`: 녹화와 재생 벤치마크, 업데이트/s·Mbps·디코딩 시간 프로파일러
- [ ] 최적화: SIMD 팔레트 탐색, 픽셀 변환, 메모리 재사용(버퍼 풀)
- **DoD:** §3.1의 성능 목표를 달성한다(벤치마크 결과를 `docs/bench.md`에 기록).

### M8. 호환성·편의 기능 (3주, P2)
- [ ] ZRLE, Hextile 인코더/디코더(noVNC, TigerVNC, RealVNC 호환)
- [ ] WebSocket(같은 포트 자동 감지, RFC 6455, binary/base64, wss), noVNC로 접속 검증
- [ ] 역방향 연결(서버 `-connect host[:5500]`, 뷰어 리슨 모드), repeater `ID:n`
- [ ] 뷰어
  - 스케일링(퍼센트/Auto/FixedRatio), 전체 화면과 다중 모니터 span
  - 키보드 그랩(Windows `WH_KEYBOARD_LL`. 원본의 `MakeLParam` 버그는 고친다.)
  - SSH 터널(`-via`, 외부 ssh), 연결 정보 파일, 옵션 저장
- [ ] `minivnc-session`
  - Xvfb와 서버 실행, 디스플레이 번호 할당, xauth, xstartup
  - `-kill`/`-list`, OTP 발급
- [ ] 스크롤 감지 기반 CopyRect(서버, 실험적)
- [ ] Windows 서버 백엔드(DXGI Desktop Duplication + SendInput) — 선택

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
| rustls에 익명 DH가 없다                                             | TLSNone/TLSVnc 미지원    | `openssl` feature로 제공하고, 기본 권장은 X509*로 한다.                                                                                          |
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
