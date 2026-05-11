# LocalVariableCollection

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**id** | **String** | The unique identifier of this variable collection. | 
**name** | **String** | The name of this variable collection. | 
**key** | **String** | The key of this variable collection. | 
**modes** | [**Vec<models::LocalVariableCollectionModesInner>**](LocalVariableCollection_modes_inner.md) | The modes of this variable collection. | 
**default_mode_id** | **String** | The id of the default mode. | 
**remote** | **bool** | Whether this variable collection is remote. | 
**is_extension** | Option<**bool**> | Whether this variable collection is an extension of another variable collection. | [optional]
**parent_variable_collection_id** | Option<**String**> | The id of the parent variable collection that this variable collection is an extension of. If this variable collection is not an extension, this value will be `undefined`. | [optional]
**root_variable_collection_id** | Option<**String**> | The id of the root variable collection in the extension chain. This is the ID of the original (non-extended) collection at the top of the parent chain. For example, if Collection C extends B which extends A (root), then `rootVariableCollectionId` is A's ID. If this variable collection is not an extension, this value will be `undefined`. | [optional]
**variable_overrides** | Option<[**std::collections::HashMap<String, std::collections::HashMap<String, models::VariableValue>>**](std::collections::HashMap.md)> | The overrides for the variables in this variable collection as a map of variable ids to a map of mode ids to variable values. | [optional]
**hidden_from_publishing** | **bool** | Whether this variable collection is hidden when publishing the current file as a library. | [default to false]
**variable_ids** | **Vec<String>** | The ids of the variables in the collection. Note that the order of these variables is roughly the same as what is shown in Figma Design, however it does not account for groups. As a result, the order of these variables may not exactly reflect the exact ordering and grouping shown in the authoring UI. | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


