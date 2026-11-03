# MiniVNC 사용자 안내서

기준일: 2026-11-03 (Asia/Seoul)  
대상: D:\dev\minivnc의 현재 소스 구현. 문서의 명령은 사용 예이며, 이번 문서 작성 과정에서 빌드하거나 실행하지 않았다.

## 1. MiniVNC의 구성과 지원 범위

MiniVNC는 TurboVNC와 상호 운용하는 Rust 기반 VNC 서버와 뷰어이다. 서버는 Linux의 기존 X11 화면을 공유하며, 뷰어는 Windows와 Linux에서 실행하도록 구현되어 있다. TurboVNC의 Xvnc처럼 X 서버 자체를 포함하지 않는다.

| 구성 요소                       | 용도                                         |
| ------------------------------- | -------------------------------------------- |
| minivnc-server                  | 기존 X11 디스플레이 공유 또는 시험 화면 제공 |
| minivnc-viewer                  | MiniVNC/TurboVNC 서버 접속과 원격 조작       |
| minivnc-passwd                  | VNC 비밀번호 파일과 Linux X11 OTP 관리       |
| scripts/start-minivnc-server.sh | Linux에서 Xvfb·데스크톱·서버 세션 실행       |
| rfb-snapshot                    | 원격 화면을 PNG로 저장                       |
| rfb-bench                       | 수신 처리량 측정 및 녹화 스트림 재생         |

현재 구현에는 RFB 3.3/3.7/3.8, Tight/JPEG·Raw, TLS/VeNCrypt, PAM 로그인, OTP, 커서·클립보드·크기 변경·한국어 키 전달, Fence/Continuous Updates가 포함된다. 서버에는 멀티스레드 Tight·Interframe Comparison·ALR 코드도 포함되어 있다.

Windows 서버 실행은 pattern/PNG 시험 소스를 위한 것이다. Windows 데스크톱 캡처 서버로 사용하지 않는다. Wayland 화면 직접 캡처, WebSocket/noVNC 직접 연결, 내장 SSH 터널, 자동 재접속, ZRLE/Hextile은 현재 안내 가능한 구현이 없다. CopyRect는 클라이언트 디코딩이 구현되어 있으며, MiniVNC 서버의 주 전송 인코딩은 Tight와 Raw이다.

## 2. 준비와 빌드

### 2.1 공통

빌드는 MiniVNC 저장소 루트 D:\dev\minivnc 또는 해당 Linux 체크아웃에서 실행한다. turbovnc 참고 소스 폴더에서 cargo 명령을 실행하지 않는다.

- Cargo.toml에 선언된 최소 Rust 버전은 1.88, edition은 2024이다.
- C 빌드 도구가 필요하다. zlib은 정적으로 빌드한다.
- 현재 Linux JPEG 백엔드는 libjpeg-turbo이며 CMake와 NASM이 필요하다.
- Windows는 기본 Rust JPEG 경로를 사용할 수 있고, rfb-codec/turbojpeg 기능으로 libjpeg-turbo를 선택한다.
- Linux는 OpenSSL 및 PAM 개발 라이브러리가 필요하다. Windows OpenSSL은 vendored 빌드이며 Perl이 필요하다.
- 실제 지원 여부는 해당 플랫폼에서의 빌드·테스트 결과로 판단한다.

```bash
cargo build --release --locked --workspace
# Windows 등에서도 libjpeg-turbo를 명시적으로 사용:
cargo build --release --locked --workspace --features rfb-codec/turbojpeg
```

출력은 기본적으로 target/release 아래에 생긴다. 아래 Linux 예시는 이 경로를 PATH에 넣은 뒤 사용한다.

```bash
export PATH="$PWD/target/release:$PATH"
```

### 2.2 Linux

Debian/Ubuntu 계열의 준비 예:

```bash
sudo apt install build-essential pkg-config cmake nasm libssl-dev libpam0g-dev
# Xvfb + xfce 세션 스크립트를 사용할 때:
sudo apt install xvfb xfce4 dbus-x11
```

기존 X11 디스플레이 공유에는 해당 디스플레이 접근 권한과 DAMAGE·XTEST·XKB 확장이 필요하다. MIT-SHM이 없으면 GetImage로 대체되며 성능이 낮아질 수 있다. XFIXES는 커서와 damage 처리, RandR은 화면 크기 변경에 관여한다. 뷰어는 창 시스템과 libxkbcommon 런타임이 필요할 수 있다.

### 2.3 Windows

MSVC 빌드 환경, Rust, Perl을 준비한다. libjpeg-turbo 기능을 사용할 경우 CMake와 NASM도 준비한다. README에서 안내하는 Strawberry Perl 경로와 OpenSSL 빌드 환경을 확인한다.

```powershell
cargo build --release --locked -p minivnc-viewer -p minivnc-passwd
# libjpeg-turbo 사용:
cargo build --release --locked -p minivnc-viewer --features rfb-codec/turbojpeg
```

실행 파일은 target\release\minivnc-viewer.exe 등이다. 긴 경로로 OpenSSL 빌드가 실패하면 짧은 체크아웃/target 경로를 사용한다. Linux 세션 스크립트는 Windows PowerShell용이 아니다.

## 3. 빠르게 시작하기

### 3.1 비밀번호 만들기

Linux:

```bash
minivnc-passwd
```

기본 경로는 ~/.vnc/passwd이다. 전체 제어 비밀번호를 입력한 뒤 보기 전용 비밀번호 생성 여부를 선택한다. 처음부터 두 비밀번호를 입력하려면 minivnc-passwd -v를 사용한다.

Windows 시험 서버 등에 사용할 파일을 현재 폴더에 만들려면:

```powershell
.\target\release\minivnc-passwd.exe .\passwd
```

프롬프트 입력은 최소 6바이트를 요구하며 VNC 비밀번호는 최대 8바이트만 사용한다. 다중 바이트 문자는 문자 수와 바이트 수가 다르다. -f는 표준 입력에서 비밀번호를 읽고 이진 비밀번호 파일을 표준 출력으로 내보내는 자동화용 모드이다. 셸의 이진 리다이렉션 동작을 모르면 대화형 파일 생성을 사용한다.

### 3.2 Linux에서 새 데스크톱 세션 실행

저장소 루트에서:

```bash
bash scripts/start-minivnc-server.sh -n 1 -g 3840x2160 -p "$HOME/.vnc/passwd"
```

이 명령은 Xvfb :1, 기본 startxfce4 데스크톱, TCP 5901의 MiniVNC 서버를 실행한다. 기본 geometry는 3840x2160이고, Xvfb의 초기 크기는 이후 원격 크기 변경의 상한이 된다.

뷰어에서:

```bash
minivnc-viewer server-host:1 -PasswordFile="$HOME/.vnc/passwd"
```

비밀번호 파일 경로는 뷰어를 실행하는 컴퓨터의 파일이다. 서버에 있는 파일을 원격 경로로 지정하는 옵션이 아니다. 파일이 없으면 -PasswordFile을 생략하고 콘솔에 표시되는 비밀번호 요청에 응답한다.

### 3.3 이미 실행 중인 X11 화면 공유

```bash
minivnc-server :2 -display :0 -rfbauth "$HOME/.vnc/passwd"
```

여기서 :2는 VNC 포트 5902를 선택하고, -display :0은 공유할 X 디스플레이를 선택한다. 두 숫자가 같을 필요는 없다. 실제 공유 계정의 DISPLAY/X 접근 권한을 사용한다. -geometry는 X11 화면 크기를 설정하는 옵션이 아니다.

### 3.4 데스크톱 환경 없이 연결 시험

서버 컴퓨터:

```bash
minivnc-server :2 -source pattern -geometry 800x600 -rfbauth passwd -securitytypes VNC -localhost
```

같은 컴퓨터의 다른 터미널:

```bash
minivnc-viewer localhost:2 -PasswordFile=passwd -SecurityTypes=VNC -DesktopSize=Server
```

이 예시는 접속을 로컬로 제한한 호환성 시험이다. 시험 소스에는 실제 데스크톱 입력 대상이 없다. PNG 파일/폴더는 -source png:shots/로 제공하며, 폴더의 PNG는 이름순·같은 크기여야 하고 1초 간격으로 전환한다.

## 4. 접속 주소와 뷰어 조작

| 주소              | 의미            |
| ----------------- | --------------- |
| server-host       | 기본 TCP 5900   |
| server-host:1     | TCP 5901        |
| server-host::6000 | 명시적 TCP 6000 |

명시적 포트는 HOST::PORT 형식을 사용한다. 현재 뷰어는 HOST:N의 N이 100 이상이면 N을 포트로 취급하지만 snapshot/bench의 해석과 다르므로 이 축약형에 의존하지 않는다.

Windows 예:

```powershell
.\target\release\minivnc-viewer.exe server-host:1 -DesktopSize=Server
```

- 창 최대화 버튼은 전체 화면으로 전환한다.
- 전체 화면에서 위쪽 가장자리로 포인터를 이동하면 최소화/복원/닫기 바가 나타난다.
- Ctrl+Alt+Shift+L은 무손실 갱신을 요청한다.
- -DesktopSize=Auto가 기본이며 창 크기에 맞춰 원격 크기 변경을 요청한다.
- -DesktopSize=Server는 서버 크기를 유지한다. 서버가 크기 변경을 거부하거나 Xvfb 상한에 도달할 수 있다.
- 한/영·한자 키는 Hangul/Hangul_Hanja 전달 경로가 구현되어 있다. 서버의 입력기와 XKB 설정도 해당 동작을 지원해야 한다.
- -ViewOnly는 뷰어의 키보드·마우스 전송을 막는 설정이다. 클립보드 수신/전송 정책은 서버 옵션으로 별도 관리한다.

## 5. 화질과 반응 속도 설정

기본 뷰어 설정은 Tight, JPEG on, Quality 95, Subsampling 1X, CompressLevel 1이다. Quality 95는 수학적인 무손실이 아니다.

```bash
# 기본 고화질:
minivnc-viewer server-host:1 -Quality=95 -Subsampling=1X
# 전송량을 줄이는 설정 예:
minivnc-viewer server-host:1 -Quality=80 -Subsampling=2X -CompressLevel=6
# 무손실 Tight:
minivnc-viewer server-host:1 -JPEG=0 -CompressLevel=1
# 비교·진단용 Raw:
minivnc-viewer server-host:1 -Encoding=Raw
```

1X는 4:4:4, 2X는 4:2:2, 4X는 4:2:0, Gray는 회색조이다. CompressLevel은 JPEG 품질과 다른 설정이다. 서버는 단색/모노/팔레트/JPEG 등을 선택하며 클라이언트가 보낸 압축 레벨을 내부 정책으로 매핑한다.

서버 측 진단·조정 예:

```bash
minivnc-server :1 -display :1 -rfbauth "$HOME/.vnc/passwd" -nthreads 4 -profile
```

-nthreads는 1~4이고 사용 가능한 CPU 수로 제한된다. -nomt는 1개이다. 기본 -deferupdate는 1ms이다. 값을 무조건 줄이면 업데이트/CPU가 증가할 수 있다.

Interframe은 기본 자동이며 CL 5 이상에서 켜는 정책이다. -interframe/-nointerframe으로 강제한다. ALR는 기본 off이고 -alr 1처럼 지정하면 JPEG로 보낸 영역을 일정 대기 후 기본 무손실로 갱신한다. -alrqual을 지정하면 갱신도 JPEG이므로 완전 무손실이 아니다. 실제 갱신 타이머는 혼잡 제어의 RTT에 따라 연장될 수 있다.

## 6. 인증과 암호화

### 6.1 방식 선택

| 서버 설정 이름            | 의미                                               |
| ------------------------- | -------------------------------------------------- |
| VNC / OTP                 | VNC 비밀번호 인증 / 한 번 사용되는 VNC 비밀번호    |
| TLSVnc / TLSOtp           | 익명 TLS 채널 + VNC/OTP 인증                       |
| X509Vnc / X509Otp         | 서버 인증서 기반 TLS + VNC/OTP 인증                |
| Plain / UnixLogin         | 사용자 이름·비밀번호 인증, Linux PAM 사용          |
| TLSPlain / X509Plain      | TLS 채널에서 PAM 사용자 인증                       |
| None / TLSNone / X509None | 사용자 인증 없음; TLS/X509 이름은 채널 방식만 표시 |

TLS*의 익명 TLS는 채널 암호화와 서버 신원 검증을 구분해야 한다. 서버 신원을 확인하려면 X509*와 신뢰하는 CA/인증서를 사용한다. VNC 자체는 화면 전송을 암호화하지 않으며 Plain/UnixLogin도 이름만으로 암호화를 제공하지 않는다.

기본 서버는 사용 가능한 인증 있는 방식을 활성화한다. 서버의 선호 순서와 보안 설정 파일이 최종 선택에 영향을 주므로, 특정 방식이 필요하면 서버와 뷰어 양쪽에 명시한다. 뷰어 기본 허용 목록에도 None 등이 포함되어 있으므로 운영 정책은 명시적 목록으로 제한할 수 있다.

### 6.2 인증서를 사용하는 예

서버에서 관리자가 준비한 PEM 인증서/키를 지정한다.

```bash
minivnc-server :1 -display :1 -rfbauth "$HOME/.vnc/passwd" \
  -securitytypes X509Vnc -x509cert server-cert.pem -x509key server-key.pem
```

뷰어에서:

```bash
minivnc-viewer server-host:1 -SecurityTypes=X509Vnc -X509CA=ca.pem
```

인증서의 호스트 이름과 접속 주소를 맞춘다. 기본 추가 CA 경로는 ~/.vnc/x509_ca.pem이고, 확인 프롬프트에서 수락한 인증서는 ~/.vnc/x509_savedcerts.pem에 저장된다. 예상하지 못한 인증서 변경은 먼저 서버의 인증서와 비교한다. 자동 수락 옵션으로 안내하지 않는다.

### 6.3 PAM과 OTP

Linux PAM 서비스 기본 이름은 minivnc이다. Debian/Ubuntu 예:

```bash
sudo cp scripts/pam.d/minivnc /etc/pam.d/minivnc
minivnc-viewer server-host:1 -SecurityTypes=X509Plain -User=myuser -X509CA=ca.pem
```

서버도 X509Plain과 인증서로 설정해야 한다. 기본 로그인 대상은 서버 실행 계정이며, ACL 사용자 추가/삭제 CLI는 구현되어 있지 않다. PAM 검증은 로그인 계정의 새 데스크톱이나 OS 세션을 생성하지 않는다.

OTP는 공유 중인 X 디스플레이에 설정한다.

```bash
minivnc-passwd -o -display :1
minivnc-passwd -o -v -display :1
minivnc-passwd -c -display :1
```

생성된 OTP는 콘솔에 출력된다. 서버의 OTP 전용 설정은 -securitytypes OTP 또는 TLSOtp/X509Otp를 사용한다. 뷰어에서는 각각 VNC/TLSVnc/X509Vnc를 선택하고 생성된 값을 입력한다. OTP라는 이름은 서버 전용이다.

### 6.4 localhost와 외부 SSH 포워딩

-localhost는 IPv4 loopback에만 bind한다. 외부 SSH 클라이언트를 사용하는 예:

```bash
# 서버:
minivnc-server :1 -display :1 -rfbauth "$HOME/.vnc/passwd" -securitytypes VNC -localhost
# 클라이언트의 별도 터미널:
ssh -N -L 15901:127.0.0.1:5901 user@server-host
# 클라이언트 뷰어:
minivnc-viewer 127.0.0.1::15901 -SecurityTypes=VNC
```

SSH는 MiniVNC 내장 기능이 아니라 외부 프로그램이다.

## 7. 주요 옵션과 운영 정책

서버 옵션은 대소문자를 구분하지 않고 -name 또는 --name 뒤에 값을 받는다. 뷰어는 -Name=value 또는 -Name value를 사용하고 boolean 단독 지정은 true이다.

| 서버 옵션                           | 동작/기본값                                           |
| ----------------------------------- | ----------------------------------------------------- |
| :N / -rfbport PORT                  | 5900+N / 명시 포트 우선; 기본 5900                    |
| -display DISPLAY / -desktop NAME    | X11 대상 / 화면 이름 MiniVNC                          |
| -rfbauth FILE / -securitytypes LIST | 비밀번호 파일 / 허용 인증 방식                        |
| -securityconf FILE                  | 기본 /etc/minivncserver-security.conf                 |
| -x509cert FILE / -x509key FILE      | 인증서/키; 키 생략 시 인증서 파일에서 읽음            |
| -viewonly                           | 모든 연결의 키·포인터 입력 무시                       |
| -alwaysshared / -nevershared        | 공유/비공유 연결 정책 강제                            |
| -disconnect / -dontdisconnect       | 비공유 접속 시 기존 연결 종료/새 접속 거부; 기본 후자 |
| -pointerlocktimeout MS              | 드래그 중 포인터 소유 시간; 기본 3000ms               |
| -noflowcontrol                      | 혼잡 제어 비활성; 진단 목적 외에는 신중히 변경        |
| -noremoteresize                     | 원격 화면 크기 변경 금지                              |
| -maxclipboard BYTES                 | 기본 1048576바이트                                    |
| -noclipboardsend / -noclipboardrecv | 데스크톱→뷰어 / 뷰어→데스크톱 클립보드 차단           |
| -noprimarysync                      | X11 PRIMARY 선택 동기화 비활성                        |

뷰어는 -Shared=0으로 비공유 접속을 요청하고 -ViewOnly로 입력 전송을 막는다. -PasswordFile은 VNC 파일, -User는 PAM 사용자 이름이다. -Password는 VNC 비밀번호를 명령행으로 전달하므로 대화형 입력이나 파일을 우선 사용한다.

보안 설정 파일은 한 줄당 지시문 하나를 사용한다. 빈 줄과 #으로 시작하는 주석을 사용할 수 있다. 같은 줄 끝 주석에 의존하지 않는다.

```text
permitted-security-types=X509Vnc
no-remote-resize
no-clipboard-recv
max-desktop-size=3840x2160
```

permitted-security-types는 CLI가 선택한 방식과 교집합으로 적용되므로 인증서 등 필수 옵션도 준비해야 한다. no-remote-connections, no-clipboard-send, pam-service-name, permitted-cipher-suites, enable-user-acl도 파서에 있다. Unix에서는 파일 소유자가 root 또는 서버 사용자여야 하고 그룹/다른 사용자 쓰기 권한이 없어야 한다.

max-idle-timeout, no-pam-sessions 등 일부 TurboVNC 지시문은 경고 후 무시된다. 해당 지시문이 적용되었다고 가정하지 말고 서버 로그를 확인한다. 완전한 목록과 제약은 개발자 매뉴얼에 정리했다.

## 8. 세션 종료·로그·환경 변수

세션 스크립트는 전경에서 실행한다. 데스크톱 로그아웃, Ctrl+C, 서버 종료 중 하나가 발생하면 해당 스크립트가 시작한 세션 전체를 정리한다. 뷰어 창을 닫는 동작은 접속 해제이며, 서버/데스크톱 로그아웃과 구분한다. 현재 서비스 등록, 재접속, 재시작 감시는 별도 운영 도구가 필요하다.

| 변수/로그                                 | 용도                                  |
| ----------------------------------------- | ------------------------------------- |
| MINIVNC_SERVER                            | 세션 스크립트가 실행할 서버 경로      |
| DESKTOP                                   | 데스크톱 실행 명령; 기본 startxfce4   |
| LOG_DIR                                   | 세션 로그 경로; 기본 ~/.cache/minivnc |
| RUST_LOG                                  | 서버/뷰어 로그 수준; 기본 info        |
| xvfb-N.log / desktop-N.log / server-N.log | 세션별 Xvfb/데스크톱/서버 로그        |

```bash
RUST_LOG=debug minivnc-viewer server-host:1
```

스크립트는 ~/.vnc/x509_cert.pem과 ~/.vnc/x509_private.pem이 모두 읽을 수 있으면 서버에 전달한다. 인증서를 강제로 선택하려면 -- 뒤의 서버 옵션에 -securitytypes X509Vnc 등도 지정한다.

## 9. 화면 저장과 성능 확인

아래 도구는 현재 VNC/None 인증으로 접속하는 경로를 사용한다. TLS 전용 서버에는 같은 명령으로 접속할 수 없으며, 도구 사용을 위해 운영 서버의 인증 정책을 낮추지 않는다. 분리된 로컬 시험 서버에서 사용한다.

```bash
rfb-snapshot -passwd passwd -preset lossless localhost:2 screen.png
rfb-bench -passwd passwd -preset perceptual -time 30 localhost:2
rfb-bench -passwd passwd -record session.rfb -time 30 localhost:2
rfb-bench -replay session.rfb -iter 3
```

snapshot 기본은 lossless, timeout은 10초이다. bench 기본은 perceptual, 측정 30초, 중간 보고 5초이다. 녹화 파일은 원격 화면 데이터 등을 포함하므로 접근 권한과 보관 범위를 관리한다. bench의 업데이트/s는 실제 모니터 FPS와 같지 않으며 replay는 창 렌더링을 측정하지 않는다.

## 10. 문제 해결

| 증상                                 | 확인할 사항                                                                  |
| ------------------------------------ | ---------------------------------------------------------------------------- |
| 연결 거부/시간 초과                  | 서버 로그의 bind 주소·포트, -localhost, 방화벽, 터널과 서버 실행 상태        |
| X 디스플레이에 연결 불가             | DISPLAY, -display, 실행 계정의 X 권한; VNC :N과 X 디스플레이 혼동            |
| no security types / 인증 방식 불일치 | 양쪽 SecurityTypes, 보안 설정 교집합, X509 인증서, Linux PAM 지원            |
| 비밀번호 실패                        | 서버/뷰어 파일 경로, 최초 8바이트, OTP 소진, view-only 비밀번호              |
| 반복 실패 후 일시 거부               | 서버의 인증 실패 차단 및 로그; 계속 재시도하지 않고 자격 증명 확인           |
| PAM 초기화 실패                      | /etc/pam.d/minivnc와 배포판 모듈, pam-service-name, 서버 실행 사용자         |
| 인증서 확인 실패                     | CA, 인증서 유효성·이름, 접속 호스트, savedcerts 파일                         |
| 화면 크기 변경 실패                  | Xvfb 초기 크기, RandR, -noremoteresize, max-desktop-size, DesktopSize=Server |
| 커서가 없거나 입력 불가              | XFIXES/XTEST/XKB, 보기 전용 정책, 시험 소스 여부                             |
| 느린 화면                            | release 빌드, SHM 대체 경로 경고, CPU·해상도·대역폭·JPEG 설정·-profile       |
| 한/영 키가 기대와 다름               | 원격 입력기 및 XKB 설정, QEMU 키 코드 협상, 키보드 포커스                    |
| 클립보드 미동작                      | 서버 양방향 정책·크기 제한·PRIMARY 설정, 원격 앱의 선택 방식                 |
| X 디스플레이 사용 중                 | 기존 세션/프로세스와 로그 확인; 활성 세션의 lock 파일을 임의 삭제하지 않음   |

버그 보고에는 OS, 빌드 옵션, 서버/뷰어 실행 인자, 화면 소스, 재현 단계, 관련 로그를 포함한다. 비밀번호·개인 키·인증 토큰·민감한 녹화 화면은 제거한다.

## 11. 문서 관계와 소스 근거

개발 구조·검증 절차는 [개발자 매뉴얼](developers-manual.md), 성능 개선 후보는 [개선 계획](improvement-plan.md)을 참고한다. 개선 계획은 분석 시점의 제안이며 이후 구현 완료를 뜻하지 않는다.

이 안내서는 bins/minivnc-server/src/main.rs, bins/minivnc-viewer/src/main.rs, bins/minivnc-passwd/src/main.rs, scripts/start-minivnc-server.sh, 서버 보안 설정/PAM 구현, 각 Cargo.toml과 tools의 CLI 소스를 기준으로 작성했다. 소스가 개발 중이므로 README의 마일스톤 문구보다 실제 파서·기능 협상·플랫폼 빌드 결과를 우선 확인한다.

이 파일은 MiniVNC 안내서이며, 저장 위치가 TurboVNC 참고 소스 아래라는 이유로 TurboVNC 자체의 사용자 문서로 해석하지 않는다.
