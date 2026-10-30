# ReviewCampaign — 정체성과 발전

## 정체성

독립 caller가 제공한 immutable subject를 여러 discovery 관점과 non-self fact check로 검토하여 evidence 기반 결과를 반환한다. caller는 review CLI/embedding host다. 일반 Agent의 숨은 실행 단계나 자동 수정기는 아니다.

## 변하면 안 되는 것

Subject identity·allow-list·provider binding, 독립 discovery/fact-check와 pure reducer 의미, review 전용 state/lease/claim 권한을 보존한다. provider/setup 실패를 finding으로 꾸미지 않는다. producer task와 workspace를 수정하지 않는다.

## 변경 가능한 것

같은 public contracts·독립성·증거 수용 기준을 만족하는 provider, 저장 mechanism, CLI host와 내부 reducer 구현은 교체할 수 있다. 교체 허용은 미승인 review policy 변경을 뜻하지 않는다.

## 발전 방향

리뷰의 독립성, source/evidence 추적 가능성과 일관된 판정의 품질을 우선한다. 일반 image visual qualification과 목적이 다르므로 state owner를 합치지 않는다. 실행과 검토를 분리한 채 public provider port를 재사용한다.
