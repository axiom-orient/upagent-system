# ReviewCampaign — 남은 검증

로컬 workspace gate·release build·CLI 검증은
[통합 구현 현황](../../../../docs/IMPLEMENTATION_STATUS.md)에 기록한다.
독립 state·readonly subject·review-only repair claim을 보존한다.

별도 실환경 qualification은 명시적 route/provider의 account-backed campaign과
취소·복구 확인이다. 실행하는 소스·바이너리 identity와 입력·출력·독립 finding·
incomplete→recovery 증거를 연결해야 한다. 로컬 fixture를 이 증거로 대체하지 않는다.

일반 Agent image recovery 작업이 independent review policy 변경의 승인은 아니다. 새 merged binary나 자동 workspace repair는 범위에 넣지 않는다.
