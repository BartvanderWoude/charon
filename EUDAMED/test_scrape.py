#!/usr/bin/env python3
"""Offline self-check of scrape.py's build and Basic UDI-DI assignment: python3 EUDAMED/test_scrape.py"""
import sqlite3
import tempfile
from argparse import Namespace
from pathlib import Path

from scrape import basic_owners, cmd_build, name, write

RISK = {"code": "refdata.risk-class.class-i"}


def row(uuid, ulid):
    return {"uuid": uuid, "ulid": f"u{uuid}", "primaryDi": f"di-{uuid}", "basicUdi": "B1", "basicUdiDiDataUlid": ulid,
            "tradeName": "Catheter", "riskClass": RISK, "deviceStatusType": {"code": "on-the-market"},
            "manufacturerSrn": "NL-MF-1"}


with tempfile.TemporaryDirectory() as tmp:
    data = Path(tmp)
    write(data / "list" / name(0), [row("a", "B"), row("b", "B")])
    write(data / "list" / name(1), [row("b", "B"), row("c", "C")])  # "b" again: offsets shifted mid-run
    write(data / "detail" / name(0), [
        {"uuid": "a", "sterile": True, "latex": False, "cndNomenclatures": [
            {"code": "C0101", "description": {"textByDefaultLanguage": None, "texts": [{"text": "needles"}]}}]},
        {"uuid": "b", "error": "HTTP 404 for ..."}])
    write(data / "basic" / name(0), [{"uuid": "bb", "ulid": "B", "basicUdi": {"code": "B1"}, "riskClass": RISK,
                                       "implantable": True, "manufacturer": {"srn": "NL-MF-1"}}])
    write(data / "actors.jsonl.gz", [{"ACTOR_ID": "NL-MF-1", "NAME": "Acme", "ACTOR_TYPE": "Manufacturer"}])

    assert basic_owners(data) == {0: [("B", "a")], 1: [("C", "c")]}, basic_owners(data)

    cmd_build(Namespace(data=data))
    db = sqlite3.connect(data / "eudamed.sqlite")
    q = lambda sql: db.execute(sql).fetchall()
    assert q("select uuid, sterile, latex, risk_class from udi_di order by uuid") == [
        ("a", 1, 0, "refdata.risk-class.class-i"), ("b", None, None, "refdata.risk-class.class-i"),
        ("c", None, None, "refdata.risk-class.class-i")]
    assert q("select * from udi_cnd") == [("a", "C0101", "needles")]
    assert q("select code, implantable, manufacturer_srn from basic_udi_di") == [("B1", 1, "NL-MF-1")]
    assert q("select srn, name from actor") == [("NL-MF-1", "Acme")]
    assert q("select json_extract(detail_json, '$.sterile') from udi_di where uuid='a'") == [(1,)]
print("ok")
