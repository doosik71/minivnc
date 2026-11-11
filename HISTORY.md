# 변경 이력

## 0.1.1 (2026-11-11)

- 뷰어: 일본어 입력기 키(전각/반각, 가나 등)와 범용 IME 키를 X keysym으로 전달
- Windows 뷰어: 콘솔 없는 GUI 프로그램으로 바꾸고, 서버·비밀번호·인증서 확인을 대화상자로 묻는다
- 뷰어: 창 크기 변경, 최소화 복원, 창 이동, 연결 바 숨김 뒤 화면 일부가 검게 남거나 바가 남던 문제 수정
- 스크립트: 빌드(`build-minivnc`)와 배포 압축 파일 생성(`create-distro`) 추가. Windows도 libjpeg-turbo를 기본으로 사용
- `minivnc-session`: 시작에 실패하면 실행하지 못한 프로그램과 로그 내용을 출력. README에 선행 조건과 설치 명령 추가

## 0.1.0 (2026-11-04)

- RFB 3.3–3.8 서버(`minivnc-server`)와 뷰어(`minivnc-viewer`). Linux X11 화면 캡처와 XTEST 입력
- 인코딩: TurboVNC 호환 Tight(JPEG, 멀티스레드, interframe, ALR), ZRLE, Hextile, Raw, CopyRect
- 보안: VncAuth, VeNCrypt TLS/X509, PAM(Plain/UnixLogin), OTP, 인증 실패가 반복되는 호스트 차단
- 프로토콜 확장(데스크톱 크기 변경, 연속 업데이트, 확장 클립보드 등), WebSocket, 역방향 연결
- 세션 관리(`minivnc-session`), 비밀번호 도구(`minivnc-passwd`), 측정 도구(`rfb-snapshot`, `rfb-bench`)
