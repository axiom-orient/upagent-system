# ReviewCampaign — SPEC

입력은 unknown field를 거절하는 ReviewRequest schemaVersion 2다. inline bounded UTF-8 subject 또는 digest/allow-list가 고정된 artifactBundle을 받는다. 정확히 neutral/critical/adversarial의 세 discoverer, 2..4 distinct fact checker, non-self 확인과 route/model 다양성 등 검증된 recipe를 따른다.

출력은 frozen candidate findings에 대한 evidence reducer 결과다. setup/provider 실패, cancellation, incomplete/recovery와 repair claim을 구분한다. source workspace와 producer task를 변경하지 않는다.

Wire fields·route file schemaVersion 5·CLI 사용법·세부 수용 조건의 단일 원본은 [ReviewCampaign 계약](../../../docs/REVIEW_CAMPAIGN.md)과 [ReviewRequest·검증 함수](../src/lib.rs)다. 제품 구현 현황은 [Current](IMPLEMENTATION_STATUS.md)에 둔다.
