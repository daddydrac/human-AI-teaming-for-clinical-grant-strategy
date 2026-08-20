use anyhow::Result;
use arrow::{array::{RecordBatch, StringArray}, datatypes::{DataType, Field, Schema}};
use parquet::arrow::ArrowWriter;
use std::{fs::File, path::Path, sync::Arc};

pub fn write_evidence_parquet(path:&Path, ids:&[String], texts:&[String], sources:&[String])->Result<()> {
    let schema=Arc::new(Schema::new(vec![Field::new("evidence_id",DataType::Utf8,false),Field::new("text",DataType::Utf8,false),Field::new("source",DataType::Utf8,false)]));
    let batch=RecordBatch::try_new(schema.clone(),vec![Arc::new(StringArray::from(ids.to_vec())),Arc::new(StringArray::from(texts.to_vec())),Arc::new(StringArray::from(sources.to_vec()))])?;
    let file=File::create(path)?; let mut w=ArrowWriter::try_new(file,schema,None)?; w.write(&batch)?; w.close()?; Ok(())
}

pub fn write_retrieval_parquet(path:&Path, records:&[crate::domain::RetrievalRecord])->Result<()> {
    use arrow::array::{Float32Array, Int64Array, UInt32Array};
    let schema=Arc::new(Schema::new(vec![
        Field::new("row",DataType::UInt32,false),
        Field::new("item_id",DataType::Utf8,false),
        Field::new("kind",DataType::Utf8,false),
        Field::new("requirement_id",DataType::Utf8,true),
        Field::new("source_ref",DataType::Utf8,false),
        Field::new("source_url",DataType::Utf8,true),
        Field::new("source_locator",DataType::Utf8,true),
        Field::new("text",DataType::Utf8,false),
        Field::new("confidence",DataType::Float32,false),
        Field::new("status",DataType::Utf8,false),
        Field::new("created_unix",DataType::Int64,true),
    ]));
    let rows=UInt32Array::from(records.iter().map(|r|r.row).collect::<Vec<_>>());
    let item=StringArray::from(records.iter().map(|r|r.item_id.as_str()).collect::<Vec<_>>());
    let kind=StringArray::from(records.iter().map(|r|r.kind.as_str()).collect::<Vec<_>>());
    let req=StringArray::from(records.iter().map(|r|r.requirement_id.as_deref()).collect::<Vec<_>>());
    let src=StringArray::from(records.iter().map(|r|r.source_ref.as_str()).collect::<Vec<_>>());
    let url=StringArray::from(records.iter().map(|r|r.source_url.as_deref()).collect::<Vec<_>>());
    let loc=StringArray::from(records.iter().map(|r|r.source_locator.as_deref()).collect::<Vec<_>>());
    let text=StringArray::from(records.iter().map(|r|r.text.as_str()).collect::<Vec<_>>());
    let conf=Float32Array::from(records.iter().map(|r|r.confidence).collect::<Vec<_>>());
    let status=StringArray::from(records.iter().map(|r|r.status.as_str()).collect::<Vec<_>>());
    let created=Int64Array::from(records.iter().map(|r|r.created_unix).collect::<Vec<_>>());
    let batch=RecordBatch::try_new(schema.clone(),vec![Arc::new(rows),Arc::new(item),Arc::new(kind),Arc::new(req),Arc::new(src),Arc::new(url),Arc::new(loc),Arc::new(text),Arc::new(conf),Arc::new(status),Arc::new(created)])?;
    let file=File::create(path)?; let mut w=ArrowWriter::try_new(file,schema,None)?; w.write(&batch)?; w.close()?; Ok(())
}
