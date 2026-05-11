# DeveloperLogResource

## Properties

Name | Type | Description | Notes
------------ | ------------- | ------------- | -------------
**id_or_key** | Option<**String**> | The ID or key of the resource. For files this is the file key; for teams and projects this is the numeric ID. Null for requests without an associated resources (e.g. activity logs). | [optional]
**name** | Option<**String**> | The name of the resource; null for requests without an associated resource (e.g. activity logs). | [optional]
**r#type** | Option<**String**> | The type of resource; null for requests without an associated resource (e.g. activity logs). | [optional]
**org_id** | **String** | The ID of the organization associated with the request (e.g. that owns the resource). | 

[[Back to Model list]](../README.md#documentation-for-models) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to README]](../README.md)


