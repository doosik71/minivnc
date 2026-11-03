# MiniVNC

**MiniVNC**는 고성능 VNC 구현체인 [TurboVNC](https://TurboVNC.org)의 핵심 기능을 **Rust**로 다시 개발하는 프로젝트입니다.

TurboVNC의 강점을 Rust로 옮겨, 메모리 안전하고 배포하기 쉬운 서버와 뷰어를 만드는 것이 목표입니다. 옮길 강점은 다음과 같습니다.
- 고속 Tight + JPEG 인코딩
- 지연 업데이트와 혼잡 제어
- 자동 무손실 갱신(ALR)

> **상태:** M5(고급 프로토콜 기능)를 마쳤습니다. 서버는 Linux에서 기존 X 디스플레이(Xvfb, Xorg)를 TurboVNC 방식의 Tight(JPEG 포함) 또는 Raw로 공유하고, 뷰어(`minivnc-viewer`)는 TurboVNC 서버(Xvnc)와 MiniVNC 서버에 접속합니다. 양쪽 모두 Fence·Continuous Updates와 TurboVNC식 혼잡 제어, 원격 화면 크기 변경, 커서 모양, 확장 클립보드, QEMU 키 코드(한/영·한자 키 → `Hangul`/`Hangul_Hanja`), LED 상태, 확장 마우스 버튼을 지원합니다. 보안 확장(TLS 등)은 M6에서 합니다. 상세 분석과 구현 계획은 [DEVPLAN.md](DEVPLAN.md)를 참고하세요.

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

| 분류      | 기능                                                                                                                            |
| --------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 프로토콜  | RFB 3.3 / 3.7 / 3.8, TightVNC 확장, Fence, Continuous Updates, ExtendedDesktopSize, QEMU Extended Key Event, Extended Clipboard |
| 인코딩    | **TurboVNC Tight** (JPEG, 팔레트, 모노, 단색 영역 분할, 멀티스레드), CopyRect, Raw, ZRLE, Hextile                               |
| 화질 제어 | JPEG 품질 1–100, 크로마 서브샘플링 1X/2X/4X/Gray, 압축 레벨 0–9, Interframe Comparison, Automatic Lossless Refresh              |
| 보안      | None, VncAuth, Tight, VeNCrypt (TLS/X509 + None/Vnc/Plain), UnixLogin/PAM, OTP, view-only 비밀번호, 무차별 대입 차단            |
| 서버      | X11 화면 캡처(XShm + XDamage), XTest 입력, RandR 크기 변경, 다중 클라이언트, WebSocket(noVNC), 역방향 연결                      |
| 뷰어      | 네이티브 창, 스케일링, 전체 화면 및 다중 모니터, 키보드 그랩, 클립보드, SSH 터널                                                |

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
- JPEG: Linux에서는 TurboVNC처럼 libjpeg-turbo 3을 씁니다. 크레이트에 든 소스를 정적으로 빌드하므로 CMake와 NASM이 필요합니다(`sudo apt install cmake nasm`; 배포판의 libjpeg-turbo 2.x에는 TurboJPEG 3 API가 없습니다). Windows 기본은 순수 Rust이고, `--features rfb-codec/turbojpeg`로 libjpeg-turbo를 쓸 수 있습니다(CMake와 NASM 필요).
- Linux X11 서버는 순수 Rust X 프로토콜 구현(x11rb)을 쓰므로 X11 개발 헤더가 필요 없습니다. X 서버에는 MIT-SHM(없으면 느린 GetImage 사용), DAMAGE, XTEST, XKEYBOARD 확장이 있어야 합니다.
- 뷰어는 winit + softbuffer로 그리며 빌드에 추가 시스템 패키지가 필요 없습니다. Linux에서는 실행할 때 X11(또는 Wayland)과 libxkbcommon 라이브러리를 불러옵니다.
- TLS(VeNCrypt)는 OpenSSL을 씁니다.
  - Linux: 시스템 OpenSSL과 서버의 PAM을 씁니다(Debian/Ubuntu: `sudo apt install libssl-dev pkg-config libpam0g-dev`).
  - Windows: OpenSSL을 소스에서 정적으로 빌드하므로 Perl이 필요합니다. [Strawberry Perl](https://strawberryperl.com)을 설치하고 Git for Windows의 perl보다 앞에 오게 하세요(또는 `OPENSSL_SRC_PERL=C:\Strawberry\perl\bin\perl.exe`). NASM이 PATH에 있으면 어셈블리 구현을 씁니다. 빌드 경로가 너무 길면 OpenSSL 구성이 "files missing"으로 실패하니, 짧은 경로(예: `--target-dir C:\t`)를 쓰세요. 링크할 때 나오는 `LNK4099`(PDB 없음) 경고는 무시해도 됩니다.

```sh
cargo build --release
cargo build --release --workspace --features rfb-codec/turbojpeg   # Windows에서 libjpeg-turbo 사용
```

## 현재 시험해 볼 수 있는 것 (M6)

```sh
# 비밀번호 파일 생성 (TurboVNC vncpasswd -f와 같은 출력)
echo secret12 | minivnc-passwd -f > passwd

# Linux: Xvfb + xfce4 세션을 한 번에 띄우는 스크립트 (포트 5901, 최대 5120x1440).
# TurboVNC vncserver처럼 로그오프하거나 Ctrl+C를 누르면 세션 전체가 끝난다.
scripts/start-minivnc-server.sh -n 1 -g 5120x1440 -p ~/.vnc/passwd

# 같은 일을 손으로 할 때 (포트 5901)
# (WSLg에서는 GTK 앱이 Wayland로 가지 않도록 WAYLAND_DISPLAY를 지운다.
#  Xvfb는 시작 크기보다 커질 수 없으므로, 뷰어 창에 맞춰 크기를 바꾸려면
#  크게 시작한다)
Xvfb :1 -screen 0 3840x2160x24 &
env -u WAYLAND_DISPLAY DISPLAY=:1 dbus-launch --exit-with-session startxfce4 &
minivnc-server :1 -display :1 -rfbauth passwd
# 보안 타입: 기본은 TurboVNC처럼 인증이 있는 것 중 쓸 수 있는 것
#   (TLSVnc, TLSOtp, TLSPlain, X509*, VNC, OTP, UnixLogin, Plain).
#   X509*는 인증서가 있어야 한다: -x509cert cert.pem -x509key key.pem
#   (시작 스크립트는 ~/.vnc/x509_cert.pem, x509_private.pem이 있으면 넘긴다)
#   Plain/UnixLogin은 PAM 서비스 minivnc를 쓴다:
#   sudo cp scripts/pam.d/minivnc /etc/pam.d/minivnc
#   보안 설정 파일: /etc/minivncserver-security.conf (TurboVNC 문법)
# 일회용 비밀번호(OTP): 서버가 도는 X 디스플레이에 설정하고 출력한다.
minivnc-passwd -o -display :1      # -v: view-only OTP도, -c: 지우기
# 그 밖의 옵션: -deferupdate MS, -viewonly, -localhost, -securitytypes none,
#   -alwaysshared / -nevershared / -disconnect, -pointerlocktimeout MS,
#   -noflowcontrol, -noremoteresize, -maxclipboard BYTES, -noclipboardsend,
#   -noclipboardrecv, -noprimarysync

# 테스트 화면 서버(모든 플랫폼): 움직이는 패턴 또는 PNG 파일/디렉터리, 포트 5902
minivnc-server :2 -rfbauth passwd -source pattern -geometry 800x600
minivnc-server :2 -rfbauth passwd -source png:shots/

# 뷰어로 접속 (Windows/Linux). 비밀번호 옵션이 없으면 필요할 때 콘솔에서 묻는다.
minivnc-viewer myhost:1
minivnc-viewer myhost:1 -PasswordFile=passwd -Quality=80 -Subsampling=2X
minivnc-viewer myhost::5901 -JPEG=0 -CompressLevel=0   # 무손실
# TLS: 기본 SecurityTypes는 TurboVNC 뷰어와 같다. X509 인증서를 확인할 수
#   없으면 콘솔에서 묻고, 수락한 인증서는 ~/.vnc/x509_savedcerts.pem에 저장한다.
minivnc-viewer myhost:1 -SecurityTypes=X509Vnc -X509CA=ca.pem
minivnc-viewer myhost:1 -User=alice                  # Unix Login / Plain
# 그 밖의 옵션: -Password=PW, -Encoding=Tight|Raw, -Shared=0, -ViewOnly,
#   -DesktopSize=Auto|Server (Auto: 창 크기에 맞춰 원격 화면 크기를 바꾼다)
# 최대화 버튼: 전체 화면. 위쪽 가장자리 가운데에 마우스를 대면
#   최소화·복원·닫기 바가 나타난다.

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
```

## 로드맵

| 단계 | 내용                                                                        |
| ---- | --------------------------------------------------------------------------- |
| M0   | 워크스페이스, 프로토콜 상수, 영역 연산                                      |
| M1   | 핸드셰이크, VncAuth, Raw 인코딩                                             |
| M2   | **Tight 인코더/디코더 (JPEG 포함)**                                         |
| M3   | X11 캡처 서버                                                               |
| M4   | 뷰어 MVP                                                                    |
| M5   | Fence/CU/흐름 제어, 데스크톱 크기 변경, QEMU 키(한/영·한자), 커서, 클립보드 |
| M6   | Tight 보안, VeNCrypt/TLS, PAM, OTP                                          |
| M7   | 멀티스레드 인코딩, Interframe Comparison, ALR, 벤치마크                     |
| M8   | ZRLE/Hextile, WebSocket, 역방향 연결, SSH 터널, 세션 관리                   |

마일스톤별 작업 항목과 완료 기준은 [DEVPLAN.md §5](DEVPLAN.md#5-단계별-구현-계획)를 참고하세요.

## 라이선스

TurboVNC(GPL v2)의 알고리즘과 데이터 테이블을 이식하므로, MiniVNC는 **GPL-2.0-or-later**로 배포합니다. 라이선스 전문은 [LICENSE](LICENSE), 이식한 TurboVNC 코드의 저작권 고지는 [NOTICE](NOTICE)를 참고하세요.

## 참고

- TurboVNC: <https://TurboVNC.org> · <https://github.com/TurboVNC/turbovnc>
- RFB 프로토콜 커뮤니티 문서: <https://github.com/rfbproto/rfbproto>
- 원본 소스 분석 메모: [turbovnc/MEMO.md](turbovnc/MEMO.md) (Windows 뷰어 한/영·한자 키 패치)
