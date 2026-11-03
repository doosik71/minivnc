# MiniVNC 개발자 매뉴얼

기준일: 2026-11-03 (Asia/Seoul)  
대상 저장소: D:\dev\minivnc  
문서 파일명: 요청된 ddevelopers-manual.md를 그대로 사용한다.

이 문서는 소스 정적 분석에 기반한다. 이번 작성 작업에서는 소스 수정, 빌드, 테스트, 서버 실행을 하지 않았다. 설명한 기능의 코드 존재와 해당 환경에서의 실행 검증을 구분한다. 다른 에이전트의 진행 중 변경을 포함한 읽기 결과이므로 고정 커밋의 릴리스 인증 문서가 아니다.

## 1. 목적과 구현 상태

MiniVNC는 TurboVNC와의 RFB 상호 운용을 목표로 한 Rust 워크스페이스이다. 서버는 기존 Linux X11 화면을 캡처하며 X 서버 구현을 포함하지 않는다. 참고 디렉터리 turbovnc는 확인 시 D:\dev\turbovnc로 연결되어 있다. 그 C/Java 소스는 Rust 워크스페이스 구성원이 아니다.

| 기능      | 확인한 현재 구현                                           | 경계                                      |
| --------- | ---------------------------------------------------------- | ----------------------------------------- |
| RFB       | 3.3/3.7/3.8, Tight/VeNCrypt 협상                           | 확장은 협상 후 사용                       |
| 인코딩    | 서버 Tight/JPEG·Raw, 클라이언트 Tight·Raw·CopyRect         | ZRLE/Hextile 미구현                       |
| JPEG      | Linux libjpeg-turbo, 다른 플랫폼 선택 기능/기본 Rust 경로  | 플랫폼별 feature 구분                     |
| 성능 기능 | Tight 최대 4작업, Interframe, ALR, 프로파일                | 처리량 목표 달성은 별도 측정 필요         |
| X11       | XDamage, SHM/GetImage, XTest, XKB, XFixes, RandR 관련 경로 | Linux 전용; Wayland 직접 캡처 없음        |
| 보안      | VNC, OTP, Tight UnixLogin, Plain, TLS/X509, PAM            | PAM 세션/동적 ACL 관리 없음               |
| 뷰어      | winit/softbuffer, 크기 변경, 커서, 클립보드, 키·마우스     | 내장 SSH·자동 재접속 없음                 |
| 도구      | snapshot, bench live/record/replay                         | TLS 전용 서버 측정 지원과 GUI 측정은 별도 |

README와 DEVPLAN의 체크박스는 코드보다 뒤처질 수 있다. 기능 지원 판단은 파서·협상 목록·분기·플랫폼 cfg·테스트를 함께 본다. improvement-plan.md의 미완료 제안도 현재 코드와 다시 대조한다.

## 2. 저장소 지도와 의존 방향

| 경로                | 책임                                                             |
| ------------------- | ---------------------------------------------------------------- |
| crates/rfb-proto    | RFB 상수·메시지·직렬화·증분 파서·보안 이름                       |
| crates/rfb-region   | 정규화된 y-x band 형태의 사각형 집합                             |
| crates/rfb-auth     | VNC DES 인증, 비밀번호 파일, challenge/Access                    |
| crates/rfb-codec    | Framebuffer, Translator, Raw/CopyRect/Tight, JPEG, 클립보드 압축 |
| crates/rfb-server   | 플랫폼 독립 서버, 세션·보안·흐름 제어, 화면 소스 trait           |
| crates/rfb-client   | 동기 I/O 클라이언트, 보안 협상·디코딩·Event                      |
| crates/capture-x11  | Linux X11 소스·입력·클립보드·OTP, 키맵                           |
| bins/minivnc-server | CLI, 시스템 보안 설정, Linux PAM 결합                            |
| bins/minivnc-viewer | CLI, 네트워크 스레드, 창·플랫폼 키 처리                          |
| bins/minivnc-passwd | 비밀번호/OTP CLI                                                 |
| tools/rfb-snapshot  | 전체 화면 PNG와 바이트 수                                        |
| tools/rfb-bench     | live 처리량, 수신 녹화, 재생 디코딩                              |
| scripts             | Xvfb/데스크톱 실행 스크립트, PAM 설정 예                         |
| turbovnc            | 원본 알고리즘/프로토콜 비교용 소스                               |

의존 방향은 대체로 proto/auth/region → codec → server/client → 플랫폼 통합·실행 파일이다. capture-x11은 server가 정의한 FrameSource/InputSink를 구현한다. server가 capture-x11에 역의존하지 않도록 플랫폼 독립성을 유지한다.

## 3. 빌드와 네이티브 의존성

Cargo.toml 기준: edition 2024, rust-version 1.88, workspace resolver 3, publish=false, 라이선스 표기 GPL-2.0-or-later이다. Rust stable과 각 플랫폼의 실제 의존성 호환성은 빌드로 확인한다.

```bash
cargo build --release --locked --workspace
cargo build --release --locked --workspace --features rfb-codec/turbojpeg
```

| 대상                | 준비 사항                                                    |
| ------------------- | ------------------------------------------------------------ |
| Linux               | C 도구, pkg-config, OpenSSL/PAM 개발 라이브러리, CMake, NASM |
| Windows             | MSVC 빌드 도구, Perl 기반 vendored OpenSSL 빌드              |
| Windows + turbojpeg | 위 항목에 CMake/NASM 추가                                    |
| X11 런타임          | 접근 가능한 X 서버 및 필요한 확장                            |
| 뷰어 런타임         | winit/softbuffer 창 시스템, Linux libxkbcommon 등            |

rfb-codec의 Linux 대상 의존성은 turbojpeg의 cmake/require-simd 기능을 사용한다. 일반 배포판의 libjpeg-turbo 2.x를 무조건 연결하는 방식이 아니라 crate에 포함된 소스로 TurboJPEG 3 API를 빌드하는 구성이다. 다른 플랫폼에서는 rfb-codec/turbojpeg 선택 기능이 백엔드를 전환한다. 기본 Rust 경로에는 jpeg-encoder의 simd 기능과 zune-jpeg가 선언되어 있다.

flate2는 zlib 백엔드를 사용하고 libz-sys는 static 기능을 사용한다. Windows rfb-server/rfb-client의 OpenSSL은 vendored이다. 소스 경로가 길어 네이티브 빌드가 실패하면 짧은 경로를 사용한다. 빌드 스크립트 변경 전에는 Cargo.toml과 lockfile을 같이 확인한다.

일반 개발 빌드와 성능 측정용 release 빌드를 혼동하지 않는다. 문서 작업을 이유로 의존성을 갱신하거나 lockfile을 재생성하지 않는다.

## 4. 서버의 수명과 동시성

### 4.1 시작 경로

bins/minivnc-server/src/main.rs:

1. parse_args로 CLI를 해석한다.
2. secconf::SecurityConfig::load로 시스템 정책을 읽는다.
3. 보안 타입을 플랫폼·명시 옵션·시스템 허용 목록으로 필터링한다.
4. pattern/PNG/X11 화면 소스와 선택 입력 sink를 생성한다.
5. Server::start로 초기 화면, 공유 상태, 캡처/입력 스레드를 구성한다.
6. IPv4 listener를 bind하고 Server::serve를 실행한다.

-localhost 또는 no-remote-connections가 있으면 127.0.0.1, 아니면 0.0.0.0에 bind한다. VNC :N은 포트 선택이고 -display는 X11 대상이다. -rfbport가 있으면 포트 선택에서 우선한다.

### 4.2 스레드와 공유 상태

- 캡처 전용 스레드: FrameSource::wait_changes/read → 공유 화면 반영 → Update 방송.
- 입력 전용 스레드: mpsc로 전달받은 InputEvent를 InputSink에 주입.
- 연결별 Tokio task: 핸드셰이크, 메시지·Update·타이머 처리, 인코딩/전송.
- Tight 작업: Rayon scope로 사각형 수평 띠를 병렬 처리.
- 공유 화면: RwLock<Screen>, 데스크톱/연결 메타데이터: Mutex.
- 변경 방송: 현재 시작 경로는 broadcast::channel(256); lag 발생 시 세션 resync.
- 화면은 원본 plain과 커서 합성 drawn 경로를 구분한다.

Tokio task라고 해서 CPU 인코딩이 자동으로 비동기화되는 것은 아니다. 현재 동기 인코딩과 Rayon 완료 대기가 세션 경로에 있으며, 화면 잠금·작업량·전송 대기를 분리해서 계측해야 한다.

### 4.3 플랫폼 확장 인터페이스

FrameSource는 Send + 'static이며 width, height, format, wait_changes(timeout), read(region, fb)를 제공한다. Changes에는 damage, resized, cursor, pointer, leds, bell, clipboard, otp가 포함된다.

InputSink도 Send + 'static이다. key, pointer, release_all_keys가 기본 책임이고 QEMU 키 코드, 클립보드, 크기 변경은 추가 메서드로 연결한다. 크기 변경 성공은 실제 결과 크기를 반환하며 소스가 Changes::resized로 반영해야 한다.

새 캡처 백엔드는 이 두 trait와 소스 선택 분기를 연결한다. 프로토콜 메시지 처리에 OS API를 직접 삽입하지 않는다. 시험용 source에는 입력 sink가 없을 수 있다.

## 5. X11 구현

capture-x11/src/x11/mod.rs의 open이 X11 연결·디스플레이 포맷·소스·입력 통합의 진입점이다.

- capture.rs: damage 감지, SHM/GetImage, 커서·포인터·LED·벨, 크기 변경, OTP 속성.
- input.rs: XTest 키/마우스 주입, XKB 기반 매핑, RandR 크기 변경.
- clipboard.rs: 선택 소유권과 클립보드 동기화.
- shm.rs: SysV 공유 메모리 생성·매핑·해제.
- keymap.rs와 qnum_table.rs: 키 조합, XT scan code와 X 키 코드 관계.

확인한 최신 capture.rs에는 XFIXES가 있을 때 NON_EMPTY damage와 서버 측 region을 사용하고, 없으면 DELTA_RECTANGLES 이벤트로 대체하는 코드가 있다. 이전 개선 계획의 “매 이벤트 union” 설명을 모든 경로의 현재 동작으로 간주하지 않는다.

SHM이 없으면 GetImage로 대체한다. XFIXES 불가는 커서 지원 등에 영향을 준다. DAMAGE, XTEST, XKB 접근 실패와 선택적 확장 실패를 구분한다. CAPTURE_POLL=200ms는 종료 확인을 위한 대기 상한이며 화면을 5FPS로 제한하는 타이머가 아니다.

기존 X11 화면의 소유권/인증을 존중하고 새 백엔드에서 전역 X 접근 권한을 자동으로 넓히지 않는다. Xvfb 초기 크기와 RandR 제약은 원격 크기 변경의 한계이다.

## 6. 서버 세션과 프로토콜 상태

rfb-server/src/session.rs는 수정/요청 영역, 클라이언트 인코딩 선택, 커서 상태, 타이머, 혼잡 제어, ALR·Interframe 상태를 연결별로 보유한다.

일반 갱신 경로:

1. 변경 영역을 modified에 반영한다.
2. 일반 요청 또는 Continuous Updates 영역을 requested에 반영한다.
3. defer/혼잡 타이머 및 Fence 상태를 확인한다.
4. modified와 requested의 교집합을 선택하고 사각형을 정리한다.
5. 필요하면 Interframe 비교, 커서/크기/LED 등의 pseudo rectangle을 구성한다.
6. Tight 또는 Raw를 인코딩하고 출력 순서를 유지해 전송한다.
7. lossy 영역과 ALR 타이머, RTT/Fence 상태를 갱신한다.

변경 시 보존할 조건:

- 지원 확장은 협상 후에만 송수신한다. proto 상수 정의만으로 구현 완료를 선언하지 않는다.
- SyncNext/BlockBefore/BlockAfter 등 Fence 의미와 메시지 순서를 유지한다.
- num_rects와 실제 사각형 개수를 맞춘다. 0xFFFF/LastRect 경로를 별도로 검증한다.
- broadcast lag 시 재동기화는 누락된 변경을 복원해야 한다.
- 리사이즈는 새 크기·stride·영역·커서/비교 상태를 일관되게 전환한다.
- 보기 전용, 공유 정책, 입력 소유권과 연결 해제를 프로토콜 경로에서 유지한다.

## 7. 픽셀·영역·Tight의 불변 조건

### 7.1 픽셀과 좌표

rfb-codec의 Framebuffer는 포맷에 따른 packed 행을 보유한다. stride와 실제 버퍼 길이를 API로 확인하고, 일반 X 이미지의 행 패딩과 혼동하지 않는다.

PixelFormat은 bpp/depth, endianness, true_colour, 채널 max/shift로 해석한다. Translator는 동일 포맷 복사·작은 포맷 lookup·RGB lookup 전략을 사용한다. 서버의 흔한 32bpp little-endian RGB888은 메모리 바이트 순서로 B,G,R,X이다. 이름만으로 RGB 바이트 배열이라고 간주하지 않는다.

프로토콜 Rect는 u16 기반이고 Region Rect는 i32 반개구간이다. to_wire_rect/to_region_rect 등 변환 경계를 사용하며, 클리핑 전에 음수/오버플로 좌표를 u16으로 캐스팅하지 않는다.

### 7.2 Region

rfb-region은 y1/x1 순서의 정규 band 형태를 유지한다. 같은 band의 x 구간은 겹치거나 맞닿지 않고, 연속 band의 동일 span은 병합한다. union/intersect/subtract 결과의 정규성을 유지해야 동등성 비교와 영역 추적이 유효하다.

collapse_if_over는 사각형 수 초과 시 외접 상자로 대체한다. 정확한 변경 영역을 과잉 영역으로 넓히는 정책이므로 성능·전송량과 함께 평가한다.

### 7.3 Tight와 JPEG

TightEncoder는 연결별 4개 zlib 스트림과 작업 설정을 유지한다. 압축 레벨에 따른 내부 정책, 단색/모노/팔레트/전체 색/JPEG 선택, LastRect 분할, 손실 영역 기록은 TurboVNC 알고리즘과 비교한다.

- 최대 작업 수는 현재 4이며 zlib 스트림 분배/회전과 출력 순서를 지킨다.
- LastRect 지원 여부가 병렬 분할 경로에 영향을 준다.
- zlib reset bit, 스트림 수명, Z_SYNC_FLUSH를 유지한다.
- JPEG 입력 포맷과 pitch는 실제 메모리 배치와 일치해야 한다.
- Tight 기본 설정의 quality=None과 뷰어 기본 JPEG Q95는 서로 다른 계층의 기본값이다.
- 서버 JPEG 1회 호출당 데이터 변환/할당 최적화와 클라이언트 디코딩 최적화를 구분한다.
- ALR의 기본 갱신은 무손실이지만 alr_quality 지정 시 JPEG이다.

인코딩 완료 후 출력만 임의 폐기하면 연결별 zlib 상태와 비교/요청 상태가 클라이언트와 어긋날 수 있다. 최신 프레임 우선 정책은 인코딩 전 영역 결합 또는 명시적인 상태 복구와 함께 설계한다.

## 8. 클라이언트 엔진과 뷰어

### 8.1 공개 클라이언트 API

rfb-client의 Client::connect(stream, &ConnectOptions)는 보안·RFB 핸드셰이크 후 초기 프레임버퍼를 만든다. Stream은 TCP/TLS 전환을 제공하고 Transport는 인증 채널 전환 인터페이스이다.

핵심 메서드:

| API                              | 역할                                                 |
| -------------------------------- | ---------------------------------------------------- |
| set_pixel_format                 | 포맷을 바꾸고 프레임버퍼 초기화; 이후 전체 갱신 필요 |
| set_encodings                    | 인코딩/확장 협상 목록 송신                           |
| request_update                   | 증분/전체 화면 요청                                  |
| read_message / read_message_with | 메시지 적용 후 Event 반환                            |
| framebuffer                      | 적용된 화면 읽기                                     |
| send                             | 클라이언트 메시지 직렬화·송신                        |
| bytes_received                   | 핸드셰이크 포함 수신 바이트 통계                     |

read_message_with의 update_started 콜백은 framebuffer update 시작 시 호출된다. 비-CU 뷰어는 이 시점에 다음 요청을 보내 파이프라이닝한다. CU는 지원 메시지 수신 후 활성화한다.

Event는 화면 변경뿐 아니라 크기·커서·포인터·LED·Fence·클립보드·벨 등을 포함한다. framebuffer update만 처리하는 자체 클라이언트는 확장 제어 메시지를 누락하지 않도록 한다.

### 8.2 뷰어 스레드와 창

main.rs는 CLI·자격 증명 콜백·Stream/Client를 구성한다. conn.rs의 RFB 스레드는 디코딩 결과의 변경 영역을 SharedScreen으로 복사한다. window.rs의 메인 스레드는 winit 이벤트와 softbuffer 표시를 담당한다.

- damage_pending 원자 플래그로 중복 알림을 억제한다. 데이터 변경 누적과 알림 수를 혼동하지 않는다.
- 네트워크 writer는 메시지가 서로 섞이지 않도록 한다.
- 포커스를 잃으면 눌린 키를 해제하고, QEMU 키 코드/keysym 경로와 한국어 키를 유지한다.
- Ctrl+Alt+Shift+L은 일시적인 무손실 설정과 전체 갱신 요청을 연결한다.
- 전체 화면, 표시 오프셋, 원격 크기 자동 요청, 커서/스크롤 바 무효화를 함께 고려한다.
- 현재 표시 경로는 가시 영역을 softbuffer에 복사한다. 부분 표시 변경은 buffer age/보존 규칙을 검증한 후 적용한다.

## 9. 인증·TLS·정책

rfb-proto/src/security.rs는 이름, wire security type, VeNCrypt subtype, 인증 Method를 정의한다. VNC/OTP는 같은 VNC wire 방식이고 OTP 이름은 서버 정책이다. 뷰어는 OTP/TLSOtp/X509Otp 대신 VNC/TLSVnc/X509Vnc를 사용한다.

rfb-server의 handshake.rs/tls.rs/security.rs는 협상·인증·TLS·실패 차단을 담당한다. 기본 실패 차단 설정은 5회, 최초 10초이며 추가 실패 시 증가하는 구현이다. 이를 외부 설정 CLI가 있다고 가정하지 않는다.

rfb-auth는 DES challenge와 TurboVNC 호환 8/16바이트 비밀번호 파일을 구현한다. Password는 최초 NUL 또는 8바이트까지 사용하고 Debug에 비밀을 출력하지 않는다. 기존 고정키 파일 형식을 강한 저장 암호화로 설명하지 않는다.

Linux PAM 통합은 bins/minivnc-server/src/pam.rs에 있다. 인증은 blocking 작업으로 호출되며 기본 사용자는 세션 소유자이다. PAM 세션 생성/계정 전환은 하지 않는다. 현재 pam crate 경로는 PAM_RHOST를 설정하지 못한다. enable-user-acl은 소유자로 시작하는 ACL 경로를 선택하지만 사용자 추가/삭제 CLI는 없다.

### 9.1 시스템 보안 설정

secconf.rs의 기본 경로는 /etc/minivncserver-security.conf이다. 파일이 없으면 추가 시스템 제한이 없는 기본값이다. Unix에서는 소유자 root/실행 사용자, 그룹·다른 사용자 쓰기 금지 조건을 검사한다.

지원 지시문:

- permitted-security-types, 이전 이름 permitted-auth-methods
- no-remote-resize, no-remote-connections
- no-clipboard-send, no-clipboard-recv
- enable-user-acl, pam-service-name
- permitted-cipher-suites, max-desktop-size

경고 후 무시하는 알려진 항목: no-reverse-connections, no-x11-tcp-connections, no-x11-host-access, no-pam-sessions, tls-key-length, max-idle-timeout. 알 수 없는 줄도 경고하며, 이를 정책 적용 성공으로 간주하지 않는다. 주석은 #으로 시작하는 줄을 사용한다.

TLS*는 익명 TLS, X509*는 인증서 기반 TLS이다. 클라이언트의 CA/호스트 검증과 사용자 수락 인증서 저장을 보존한다. 인증서 오류를 성능 목적으로 우회하지 않는다. TLS reader는 대기 시 잠금 해제를 시도하지만 부분/제어 레코드의 동시성은 별도 검증 대상이다.

## 10. 파서·오류·unsafe 경계

rfb-proto::Parsed<T>:

| 결과                        | 의미                                  |
| --------------------------- | ------------------------------------- |
| Ok(Some((value, consumed))) | 메시지 완성; consumed만 소비          |
| Ok(None)                    | 데이터 부족; 논리적으로 소비하지 않음 |
| Err(ProtoError)             | 잘못된 프로토콜; 연결 오류로 처리     |

길이 제한은 할당/압축 해제 전에 적용하고, 클립보드·핸드셰이크 문자열·JPEG 크기·사각형 경계를 각각 검증한다. 성공 시 consumed가 입력 길이를 넘지 않아야 한다. 일부 입력을 받았다는 이유만으로 영구 상태를 먼저 바꾸지 않는다.

CodecError/DecodeError, ClientError, std::io::Error의 역할을 유지한다. 연결의 EOF/부분 메시지/해제와 잘못된 wire 데이터를 구분하고 민감한 payload를 로그에 남기지 않는다.

워크스페이스는 unsafe_code=deny이지만 다음 파일은 명시적인 FFI 경계 예외이다.

- capture-x11/src/x11/shm.rs: SysV 메모리 매핑과 슬라이스 수명.
- rfb-codec/src/tight/zstream.rs: zlib FFI, allocator, 출력 길이와 포인터 관리.

unsafe impl Send, native handle 수명, 재할당 후 포인터, 크기 계산을 변경하면 별도 검토한다. “전체가 unsafe 없는 순수 Rust”라고 문서화하지 않는다. jpeg/zlib/OpenSSL/PAM도 네이티브 의존성을 포함한다.

## 11. 개발 검증과 CI

실행은 저장소 루트에서 한다. 아래는 현재 CI에 선언된 검사이다.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI는 ubuntu-latest/windows-latest와 Rust stable을 사용한다. 네이티브 의존성 설치 상태와 선택 feature 조합까지 모두 검증되었다는 뜻은 아니다. 이번 문서 작성에서는 어느 검사도 실행하지 않았다.

변경 범위에 따라 좁은 검사를 먼저 실행한다.

```bash
cargo test -p rfb-proto
cargo test -p rfb-region
cargo test -p rfb-codec
cargo test -p rfb-client --test loopback
cargo test -p rfb-codec --features turbojpeg
```

확인한 통합 테스트:

| 파일                                   | 목적                                 |
| -------------------------------------- | ------------------------------------ |
| rfb-proto/tests/parse_arbitrary.rs     | 임의 입력 파서 안정성과 소비 길이    |
| rfb-proto/tests/consts_match_header.rs | 참고 rfbproto.h 상수와 비교          |
| rfb-region/tests/region_props.rs       | 영역 연산 속성                       |
| rfb-codec/tests/tight_roundtrip.rs     | 포맷·Tight 왕복                      |
| rfb-client/tests/loopback.rs           | 서버/클라이언트 연결 통합            |
| 각 모듈의 단위 테스트                  | CLI·인증·보안 설정·키맵·흐름 제어 등 |

proptest 기반 임의 입력 검사는 존재하지만 독립 cargo-fuzz 운영이 완료되었다고 설명하지 않는다. proptest-regressions 파일은 재현 입력이므로 임의로 삭제하지 않는다.

수동 상호 운용은 로컬 시험 소스와 TurboVNC 양방향 접속, 무손실 픽셀 비교, JPEG 화질·바이트, CU/Fence, 리사이즈, 커서, 한국어 키, 클립보드, TLS·PAM·OTP를 필요 범위에 따라 수행한다. 사용자 계정/인증서가 필요한 검증은 미실행 여부를 보고한다.

## 12. 벤치마크와 진단

rfb-bench의 live는 updates/s, Mpixels/s, Mbits/s를 집계하고 초기 전체 화면 이후를 측정한다. -record는 서버 수신 데이터를 저장하고 -replay는 decode 중심 재생을 반복한다. 이 녹화는 안전하게 무해화된 공개 자료가 아니다.

```bash
rfb-bench -passwd passwd -preset perceptual -time 30 -interval 5 localhost:2
rfb-bench -passwd passwd -preset medium -record session.rfb -time 30 localhost:2
rfb-bench -replay session.rfb -iter 3
```

현재 snapshot/bench는 Vnc/None을 선택하며 TLS 전용 연결 측정 도구가 아니다. CLI 주소 해석도 뷰어와 완전히 같지 않으므로 명시 포트는 HOST::PORT로 통일한다.

서버 -profile의 encode 표기는 전송 대기를 포함할 수 있으므로 순수 인코더 CPU 시간으로 해석하지 않는다. replay는 창·softbuffer 비용을 포함하지 않고, live 업데이트/s는 표시 FPS와 다르다.

성능 회귀 보고에는 commit/변경 상태, CPU/코어 제한, release/feature, libjpeg-turbo/SIMD, 해상도·소스·화질·CL·ALR/Interframe, TLS/네트워크, 클라이언트 수, p50/p95 지연과 RSS를 적는다. 서버 CPU와 외부 Xvfb/Xorg CPU도 구분한다. 자세한 개선 후보는 [성능 개선 계획](improvement-plan.md)을 현재 코드와 대조한다.

## 13. 변경 절차와 문서 유지

- AGENTS.md의 정밀 변경 원칙을 따른다. 다른 에이전트의 변경을 덮어쓰거나 문서 작업 때문에 소스를 정리하지 않는다.
- CLI 추가/변경은 parse_args, USAGE, 기본값, 범위 검사, 테스트와 사용자 안내서를 함께 갱신한다.
- 프로토콜 추가는 proto 상수·파서·writer·길이 제한 → 협상 → 세션/클라이언트 → UI/도구 → 상호 운용 순서로 추적한다.
- 새 인코딩은 서버 설정 목록과 뷰어 광고 목록, 인코더/디코더, LastRect 개수, 클라이언트별 상태를 함께 점검한다.
- 플랫폼 코드는 cfg 경계를 지키고 Windows 기본 JPEG/선택 turbojpeg 및 Linux 네이티브 경로를 구분한다.
- README의 목표·DEVPLAN의 계획·실제 구현·실행 검증을 별도 상태로 관리한다.
- 배포 시 저장소 LICENSE/NOTICE와 포함된 네이티브 라이브러리의 고지를 함께 확인한다. 이 문서는 새 라이선스를 부여하지 않는다.

Python 도구를 추가로 사용할 경우 저장소 지침은 uv를 사용하고 pip 설치를 금지한다. 현재 문서의 Rust/Cargo 작업을 위해 Python 의존성을 도입할 필요는 없다.

### 마무리 점검표

- [ ] 대상 OS별 release 빌드 및 실제 실행 결과 기록
- [ ] fmt/clippy/test와 필요한 feature 조합 결과 기록
- [ ] MiniVNC ↔ TurboVNC 양방향 호환성 확인
- [ ] 인증/암호화/보기 전용/클립보드/크기 정책 확인
- [ ] 리사이즈·느린 연결·연결 종료·장시간 메모리 확인
- [ ] 비밀번호/인증서/녹화 데이터가 배포물에 섞이지 않는지 확인
- [ ] CLI 도움말과 사용자 문서의 기본값/제한 일치
- [ ] 미구현 항목과 미검증 항목을 릴리스 노트에 명시

위 체크박스는 이번 작업의 실행 결과가 아니라 향후 마무리 작업 목록이다.

## 14. 관련 문서와 소스 탐색

- [사용자 안내서](users-guide.md): 설치, 실행, 옵션, 보안 설정, 문제 해결.
- [성능 개선 계획](improvement-plan.md): 병목 가설, 우선순위와 검증 계획.
- 저장소 루트 README.md: 목표와 기본 실행 예.
- 저장소 루트 DEVPLAN.md: 이식 분석과 마일스톤 계획.
- 저장소 루트 AGENTS.md: 변경/검증/커밋 규칙.
- turbovnc/unix/Xvnc/programs/Xserver/hw/vnc의 tight.c, rfbserver.c, flowcontrol.c, auth.c: 원본 비교.
- turbovnc/java/com/turbovnc/rfb 및 common/turbojpeg-jni: 원본 뷰어/네이티브 JPEG 비교.

경로는 MiniVNC 루트 기준이다. 이 문서가 놓인 turbovnc/docs에서 ../../crates 같은 상대 경로는 디렉터리 링크 때문에 다른 저장소를 가리킬 수 있으므로, 소스 탐색은 D:\dev\minivnc를 기준으로 한다.
