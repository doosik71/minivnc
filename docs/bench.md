# MiniVNC 성능 측정 기록

M7(성능 기능)의 완료 기준을 확인한 측정이다.

- 비교 대상: TurboVNC 3.3.1 Xvnc와 MiniVNC 서버
- 측정일: 2026-11-04. 개선 과정의 중간 수치는 2026-11-03에 측정했다.
- 완료 기준(DEVPLAN §3.1): 같은 조건에서 TurboVNC 대비 인코딩 처리량 80% 이상, 대역폭 ±10% 이내.

## 1. 결론

- **처리량**: 모든 조건에서 TurboVNC의 81~138%였다.
  - 가장 낮은 조건은 x11perf 텍스트 부하(81~83%)다. MiniVNC가 같은 시간에 2.4배 많은 픽셀을 보냈는데, 그 때문으로 본다(4.2).
  - 영상 무손실 CL0은 86%였다. 쓰기 대기를 빼면 119%다.
- **대역폭**: 영상은 모든 조건에서 TurboVNC의 0.96~1.01배, 텍스트는 Mpixel당 1.01배였다.
- **완료 기준**: 두 기준을 모두 만족한다.
- **남은 차이는 CPU**: Xvfb와 MiniVNC 서버를 합친 CPU가 영상에서 Xvnc의 약 1.4~2배(15~20% 대 8~14%)였다. MiniVNC는 기존 X 서버의 화면을 복사해 오는 구조이므로, X 서버 안에서 인코딩하는 TurboVNC보다 캡처 복사가 더 든다. zlib 무손실(CL6)에서는 반대로 MiniVNC가 더 적게 썼다(83% 대 103%).

## 2. 환경과 방법

### 2.1 환경

- **하드웨어/OS**: Windows 11의 WSL2 Ubuntu 26.04. 24 vCPU, 62 GB.
- **서버**:
  - TurboVNC 3.3.1 Xvnc
  - MiniVNC 서버(Xvfb 캡처, libjpeg-turbo 3 정적 빌드, 인코딩 스레드 4)
- **공통 조건**: 둘 다 1920x1080x24, `-deferupdate 1`, 인증 없음.
- **클라이언트**: 같은 WSL 안의 `rfb-bench`(release)다. TurboVNC 뷰어처럼 CU와 Fence를 켜고, 같은 SetEncodings 목록을 쓴다. 측정 시간은 15초다.

### 2.2 부하

| 이름    | 내용                                                                                              |
| ------- | ------------------------------------------------------------------------------------------------- |
| mandel  | mpv가 1280x720, 30fps의 망델브로 확대 영상을 재생한다(화면 전체가 매 프레임 바뀌는 사진형 화면).  |
| testsrc | mpv가 1280x720, 30fps의 ffmpeg `testsrc2`를 재생한다(색 막대, 그라데이션, 글자가 섞인 합성 화면). |
| term    | `x11perf -ftext`가 코어 글꼴 텍스트를 계속 그린다(작은 변경이 많은 텍스트 화면, X 서버 CPU 포화). |

영상은 ffmpeg로 만든 20초짜리 결정적 파일이며, 만드는 명령은 `scripts/bench-compare.sh` 머리말에 있다.

### 2.3 지표

- **처리량**: 두 서버 프로파일의 업데이트당 시간을 업데이트당 픽셀 수로 나눈 ms/Mpixel이다.
  - TurboVNC는 `TVNC_PROFILE=1`의 "Encode", MiniVNC는 `-profile`의 "encode"를 쓴다.
  - 둘 다 소켓 쓰기를 기다린 시간을 포함한다. MiniVNC는 쓰기를 뺀 시간도 따로 기록한다.
  - 표의 값은 TurboVNC 시간 ÷ MiniVNC 시간이므로 클수록 MiniVNC가 빠르다.
- **대역폭**: `rfb-bench`가 받은 바이트(Mbit/s)의 비(MiniVNC ÷ TurboVNC)다. 두 서버가 보낸 픽셀 수가 거의 같은 영상에서 그대로 비교한다. 텍스트는 Mpixel당 바이트로 비교한다.
- **서버 CPU**: 측정 15초 동안 `/proc/PID/stat`의 utime + stime이다. MiniVNC는 X 서버(Xvfb)를 더한 값이다.
- **업데이트 수**는 비교 지표로 쓰지 않는다. MiniVNC는 화면 변경을 더 크게 묶어 보내므로(초당 약 35~40회, 업데이트당 픽셀 약 2배) 같은 화면 변화에서도 TurboVNC(초당 70~90회)보다 적다.

### 2.4 재현

```sh
# TurboVNC가 /opt/TurboVNC에 없으면 XVNC로 지정한다.
cargo build --release -p minivnc-server -p rfb-bench
for w in mandel testsrc term; do
  for p in perceptual medium low lossless lossless-zlib; do
    for s in turbovnc minivnc; do scripts/bench-compare.sh $s $w $p 15; done
  done
done
```

## 3. 결과

### 3.1 서버 (TurboVNC 3.3.1 대비)

| 부하    | 프리셋                    | 처리량               | 대역폭        | 서버 CPU (MiniVNC / TurboVNC) |
| ------- | ------------------------- | -------------------- | ------------- | ----------------------------- |
| mandel  | perceptual (Q95, 1X, CL1) | 106%                 | 0.97          | 20% / 14%                     |
| mandel  | medium (Q80, 2X, CL6)     | 89%                  | 0.98          | 17% / 10%                     |
| mandel  | low (Q30, 4X, CL7)        | 100%                 | 0.97          | 15% / 10%                     |
| mandel  | lossless (CL0)            | 86% (쓰기 제외 119%) | 1.00          | 16% / 8%                      |
| mandel  | lossless-zlib (CL6)       | 138%                 | 1.00          | 83% / 103%                    |
| testsrc | perceptual                | 121%                 | 0.96          | 17% / 12%                     |
| testsrc | medium                    | 104%                 | 0.99          | 16% / 11%                     |
| testsrc | low                       | 106%                 | 1.00          | 18% / 13%                     |
| testsrc | lossless                  | 95%                  | 1.00          | 17% / 11%                     |
| testsrc | lossless-zlib             | 131%                 | 1.01          | 42% / 40%                     |
| term    | perceptual                | 83%                  | Mpixel당 1.01 | 106% / 100%                   |
| term    | lossless                  | 81%                  | Mpixel당 1.01 | 105% / 100%                   |

- lossless 두 행은 4.1의 마지막 수정 뒤에 다시 측정한 값이다(두 번의 평균).
- **term의 중간 프리셋**: medium·low·lossless-zlib는 CL 5 이상이라 Interframe 비교가 켜진다. x11perf는 같은 글자를 다시 그리므로 거의 모든 픽셀이 걸러진다(초당 0.1~0.8 Mpixel). 이때 갱신 수는 MiniVNC가 초당 101회, TurboVNC가 69~74회였고, 전송량은 둘 다 0.1 Mbit/s 미만이었다.
- **term의 갱신 수**: MiniVNC 초당 101회(26 Mpixel/s), TurboVNC 초당 42~51회(11~13 Mpixel/s)였다.
- **영상의 갱신 수**: 두 서버 모두 초당 27.6 Mpixel(영상 30fps 전체)을 보냈다.

### 3.2 뷰어 디코딩 (같은 녹화 재생)

MiniVNC 서버에서 `rfb-bench -record`로 10초씩 녹화하고, 개선 전 커밋과 개선 후 빌드의 `rfb-bench -replay`로 각각 3번 재생했다(Linux, libjpeg-turbo).

| 녹화                                    | 개선 전                      | 개선 후                      |
| --------------------------------------- | ---------------------------- | ---------------------------- |
| mandel perceptual (JPEG, 329 업데이트)  | 4.15 ms/update, 204 Mpixel/s | 2.60 ms/update, 326 Mpixel/s |
| testsrc lossless (무손실, 354 업데이트) | 1.68 ms/update, 468 Mpixel/s | 1.51 ms/update, 521 Mpixel/s |

- 재생에는 창 렌더링이 포함되지 않는다.
- 뷰어 부분 표시는 별도로 확인했다. Windows에서 움직이는 패턴을 띄우고 캡처해 보니 잔상이 없었고, 5초 동안 부분 표시가 약 300회 일어났다.

## 4. 개선 경과

### 4.1 M7 중의 변경과 효과

모두 mandel perceptual(MiniVNC) 기준이며, 텍스트는 term lossless다.

| 단계                                                                                   | 인코딩 (ms/Mpixel, 쓰기 포함) | 텍스트 갱신 (초당) |
| -------------------------------------------------------------------------------------- | ----------------------------- | ------------------ |
| M7 기능 구현 직후(순수 Rust JPEG)                                                      | 4.44                          | 5.7                |
| JPEG 입력 무변환 + jpeg-encoder AVX2                                                   | 3.63                          | —                  |
| Linux libjpeg-turbo 3                                                                  | 1.76                          | —                  |
| X 이미지 읽기 파이프라이닝(손상 이벤트 `NON_EMPTY`)                                    | —                             | 68                 |
| 팔레트 색인 교체(무손실 testsrc 45% → 92%)                                             | —                             | —                  |
| `improvement-plan.md` 3.2 적용(stride, 핸들 재사용, 서버 픽셀 색 계산, 포인터 조회 등) | 1.41                          | 101                |

- 기준 프로파일: 기능 구현 직후 시간의 75%가 순수 Rust JPEG 인코더에 있었다.
- 마지막 단계: 개선 전 커밋과 같은 시간대에 번갈아 비교했다. perceptual은 약 10% 빨라졌고, 무손실은 측정 잡음(±5%) 안에서 같았다.

### 4.2 해석

- **텍스트 부하**: TurboVNC는 x11perf와 같은 프로세스(X 서버)에서 인코딩하므로 그리기와 인코딩이 CPU를 나눈다. MiniVNC는 Xvfb가 그리는 동안 다른 프로세스에서 인코딩하므로 같은 시간에 더 많은 화면을 보낸다. 그만큼 업데이트당 픽셀이 다르고 텍스트 비중도 달라, Mpixel당 시간(81~83%)은 대략적인 비교로 본다.
- **무손실 CL0**: 업데이트당 약 2 MB를 보내므로 소켓 쓰기 대기가 크다. 인코딩 자체(쓰기 제외)는 TurboVNC보다 빠르다.
- **측정 편차**: 같은 조건을 반복하면 ±5~8% 흔들린다. 80% 경계에 가까운 조건(term)은 반복해도 81~98% 범위였다.

## 5. 한계

- 한 대의 머신에서 루프백으로 측정했다. 실제 네트워크(대역폭·지연 제한)와 다중 클라이언트는 측정하지 않았다.
- 4K 화면, Xorg(물리 디스플레이) 캡처, TLS 사용 시는 측정하지 않았다. `rfb-bench`는 인증 없음과 VNC 인증만 쓴다.
- Windows 뷰어의 디코딩은 순수 Rust JPEG(zune-jpeg)다. 위 재생 수치는 Linux(libjpeg-turbo)다.
