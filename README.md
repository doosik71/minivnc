# MiniVNC

**MiniVNC**는 고성능 VNC 구현체인 [TurboVNC](https://TurboVNC.org)의 핵심 기능을 **Rust**로 다시 개발하는 프로젝트입니다.

TurboVNC의 강점을 Rust로 옮겨, 메모리 안전하고 배포하기 쉬운 서버와 뷰어를 만드는 것이 목표입니다. 옮길 강점은 다음과 같습니다.
- 고속 Tight + JPEG 인코딩
- 지연 업데이트와 혼잡 제어
- 자동 무손실 갱신(ALR)

> **상태:** M3(X11 서버)를 마쳤습니다. Linux에서 기존 X 디스플레이(Xvfb, Xorg)를 TurboVNC 방식의 Tight(JPEG 포함) 또는 Raw로 공유하고, 키보드·마우스 입력을 XTest로 주입합니다. Xvfb + xfce4 세션을 TurboVNC Java 뷰어로 쓸 수 있습니다(타이핑, 창 이동, 동영상 재생). 커서 인코딩·화면 크기 변경(M5)과 뷰어 GUI(M4)는 아직 없습니다. 상세 분석과 구현 계획은 [DEVPLAN.md](DEVPLAN.md)를 참고하세요.

---

## 목표

- **TurboVNC와 상호 운용**
  - MiniVNC 서버에 기존 TurboVNC 뷰어로 접속할 수 있어야 합니다.
  - MiniVNC 뷰어로 기존 TurboVNC 서버(Xvnc)에 접속할 수 있어야 합니다.
- **성능:** TurboVNC 수준의 인코딩 처리량과 대역폭 효율을 목표로 합니다.
- **안전성:** Rust로 작성하고, 모든 네트워크 입력 파서를 fuzz 테스트합니다.
- **크로스 플랫폼:** 뷰어는 Windows와 Linux, 서버는 Linux(X11)를 먼저 지원합니다.
- **한국어 키보드:** 한/영 키와 한자 키를 원격 서버에 `Hangul`/`Hangul_Hanja`로 전달하는 기능을 기본으로 내장합니다.

## 주요 기능 (계획)

| 분류 | 기능 |
|---|---|
| 프로토콜 | RFB 3.3 / 3.7 / 3.8, TightVNC 확장, Fence, Continuous Updates, ExtendedDesktopSize, QEMU Extended Key Event, Extended Clipboard |
| 인코딩 | **TurboVNC Tight** (JPEG, 팔레트, 모노, 단색 영역 분할, 멀티스레드), CopyRect, Raw, ZRLE, Hextile |
| 화질 제어 | JPEG 품질 1–100, 크로마 서브샘플링 1X/2X/4X/Gray, 압축 레벨 0–9, Interframe Comparison, Automatic Lossless Refresh |
| 보안 | None, VncAuth, Tight, VeNCrypt (TLS/X509 + None/Vnc/Plain), UnixLogin/PAM, OTP, view-only 비밀번호, 무차별 대입 차단 |
| 서버 | X11 화면 캡처(XShm + XDamage), XTest 입력, RandR 크기 변경, 다중 클라이언트, WebSocket(noVNC), 역방향 연결 |
| 뷰어 | 네이티브 창, 스케일링, 전체 화면 및 다중 모니터, 키보드 그랩, 클립보드, SSH 터널 |

### TurboVNC와의 차이

- TurboVNC 서버(Xvnc)는 X 서버 전체를 포함합니다. MiniVNC 서버는 **기존 X 디스플레이(Xvfb/Xorg)를 캡처**하는 방식입니다. 헤드리스 세션이 필요하면 `minivnc-session`이 Xvfb를 띄워 연결합니다.
- 다음 기능은 범위에서 제외합니다.
  - VirtualGL/GLX/DRI3 통합
  - TightVNC 파일 전송
  - RRE/CoRRE/Zlib/ZYWRLE 인코딩
  - Java 기반 뷰어

## 프로젝트 구조 (예정)

```text
minivnc/
├── crates/
│   ├── rfb-proto/        # RFB 상수, 메시지, 픽셀 포맷
│   ├── rfb-codec/        # Tight / ZRLE / Hextile / Raw 인코더·디코더
│   ├── rfb-auth/         # VncAuth, VeNCrypt, TLS, 비밀번호 파일
│   ├── rfb-region/       # 사각형 영역 연산
│   ├── rfb-server/       # 플랫폼 독립 서버 엔진
│   ├── rfb-client/       # 플랫폼 독립 클라이언트 엔진
│   ├── capture-x11/      # X11 캡처 및 입력 주입
│   └── capture-windows/  # Windows 캡처 (선택)
├── bins/
│   ├── minivnc-server/
│   ├── minivnc-viewer/
│   ├── minivnc-passwd/
│   └── minivnc-session/
├── tools/                # rfb-snapshot, rfb-record, rfb-bench
├── turbovnc/             # 참고용 TurboVNC 3.3.2 원본 소스 (분석 대상)
├── DEVPLAN.md            # 분석 및 구현 계획
└── README.md
```

## 빌드

요구 사항:
- Rust stable (1.88 이상)
- C 컴파일러: zlib을 소스에서 정적 빌드합니다(Windows는 MSVC, Linux는 gcc/clang).
- 선택: libjpeg-turbo JPEG 백엔드(`--features rfb-codec/turbojpeg`)를 쓰려면 CMake와 NASM이 필요합니다. 기본 JPEG 백엔드는 순수 Rust입니다.
- Linux X11 서버는 순수 Rust X 프로토콜 구현(x11rb)을 쓰므로 X11 개발 헤더가 필요 없습니다. X 서버에는 MIT-SHM(없으면 느린 GetImage 사용), DAMAGE, XTEST, XKEYBOARD 확장이 있어야 합니다.

```sh
cargo build --release
cargo build --release --workspace --features rfb-codec/turbojpeg   # libjpeg-turbo 사용
```

## 현재 시험해 볼 수 있는 것 (M3)

```sh
# 비밀번호 파일 생성 (TurboVNC vncpasswd -f와 같은 출력)
echo secret12 | minivnc-passwd -f > passwd

# Linux: Xvfb + xfce4 세션을 공유, 포트 5901
# (WSLg에서는 GTK 앱이 Wayland로 가지 않도록 WAYLAND_DISPLAY를 지운다)
Xvfb :1 -screen 0 1920x1080x24 &
env -u WAYLAND_DISPLAY DISPLAY=:1 dbus-launch --exit-with-session startxfce4 &
minivnc-server :1 -display :1 -rfbauth passwd
# 그 밖의 옵션: -deferupdate MS, -viewonly, -localhost, -securitytypes none,
#   -alwaysshared / -nevershared / -disconnect, -pointerlocktimeout MS

# 테스트 화면 서버(모든 플랫폼): 움직이는 패턴 또는 PNG 파일/디렉터리, 포트 5902
minivnc-server :2 -rfbauth passwd -source pattern -geometry 800x600
minivnc-server :2 -rfbauth passwd -source png:shots/

# 아무 VNC 서버에 접속해 화면을 PNG로 저장하고 받은 바이트 수를 출력
# (프리셋: lossless, lossless-zlib, perceptual, medium, low, raw)
rfb-snapshot -passwd passwd -preset medium localhost:2 screen.png
```

## 사용 예 (예정)

```sh
# 비밀번호 파일 생성 (TurboVNC ~/.vnc/passwd 형식과 호환)
minivnc-passwd ~/.vnc/passwd

# 헤드리스 세션 시작 (Xvfb :1 + 서버, 포트 5901)
minivnc-session -geometry 1920x1080

# 기존 X 디스플레이 공유
minivnc-server -display :0 -rfbport 5900 -rfbauth ~/.vnc/passwd

# 뷰어로 접속 (TurboVNC 스타일 옵션)
minivnc-viewer myhost:1 -Quality=80 -Subsampling=2X
```

## 로드맵

| 단계 | 내용 |
|---|---|
| M0 | 워크스페이스, 프로토콜 상수, 영역 연산 |
| M1 | 핸드셰이크, VncAuth, Raw 인코딩 |
| M2 | **Tight 인코더/디코더 (JPEG 포함)** |
| M3 | X11 캡처 서버 |
| M4 | 뷰어 MVP |
| M5 | Fence/CU/흐름 제어, 데스크톱 크기 변경, QEMU 키(한/영·한자), 커서, 클립보드 |
| M6 | Tight 보안, VeNCrypt/TLS, PAM, OTP |
| M7 | 멀티스레드 인코딩, Interframe Comparison, ALR, 벤치마크 |
| M8 | ZRLE/Hextile, WebSocket, 역방향 연결, SSH 터널, 세션 관리 |

마일스톤별 작업 항목과 완료 기준은 [DEVPLAN.md §5](DEVPLAN.md#5-단계별-구현-계획)를 참고하세요.

## 라이선스

TurboVNC(GPL v2)의 알고리즘과 데이터 테이블을 이식하므로, MiniVNC는 **GPL-2.0-or-later**로 배포합니다. 라이선스 전문은 [LICENSE](LICENSE), 이식한 TurboVNC 코드의 저작권 고지는 [NOTICE](NOTICE)를 참고하세요.

## 참고

- TurboVNC: <https://TurboVNC.org> · <https://github.com/TurboVNC/turbovnc>
- RFB 프로토콜 커뮤니티 문서: <https://github.com/rfbproto/rfbproto>
- 원본 소스 분석 메모: [turbovnc/MEMO.md](turbovnc/MEMO.md) (Windows 뷰어 한/영·한자 키 패치)
