# Unicode 데이터 고지

Unicode 16.0.0 공식 데이터와 전체 Unicode License v3 고지문을 보관한다.
`sources.json`은 각 원본의 URL·SHA-256·다운로드 일자를 고정한다.

- 원본: `16.0.0/UnicodeData.txt`, `Blocks.txt`, `NameAliases.txt`, `SpecialCasing.txt`, `ReadMe.txt`
- 라이선스: `LICENSE.txt` — **Unicode-3.0**
- 생성기: `tools/generate_unicode_names.py`
- 파생 내장 데이터: `src/unicode_names/data/*.bin`

원본·파생 데이터 또는 이를 포함한 바이너리를 배포할 때 `LICENSE.txt`의 전체 저작권·허가
고지문을 같이 제공한다. 생성 규칙, 구현 출처, 동작 차이와 검사 범위는 `docs/unicode-data.md`에 있다.
JDK/Minecraft에서 추출한 이름 테이블은 포함하지 않는다.
