# \DevResourcesApi

All URIs are relative to *https://api.figma.com*

Method | HTTP request | Description
------------- | ------------- | -------------
[**delete_dev_resource**](DevResourcesApi.md#delete_dev_resource) | **DELETE** /v1/files/{file_key}/dev_resources/{dev_resource_id} | Delete dev resource
[**get_dev_resources**](DevResourcesApi.md#get_dev_resources) | **GET** /v1/files/{file_key}/dev_resources | Get dev resources
[**post_dev_resources**](DevResourcesApi.md#post_dev_resources) | **POST** /v1/dev_resources | Create dev resources
[**put_dev_resources**](DevResourcesApi.md#put_dev_resources) | **PUT** /v1/dev_resources | Update dev resources



## delete_dev_resource

> delete_dev_resource(file_key, dev_resource_id)
Delete dev resource

Delete a dev resource from a file

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | The file to delete the dev resource from. This must be a main file key, not a branch key. | [required] |
**dev_resource_id** | **String** | The id of the dev resource to delete. | [required] |

### Return type

 (empty response body)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_dev_resources

> models::InlineObject36 get_dev_resources(file_key, node_ids)
Get dev resources

Get dev resources in a file

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**file_key** | **String** | The file to get the dev resources from. This must be a main file key, not a branch key. | [required] |
**node_ids** | Option<**String**> | Comma separated list of nodes that you care about in the document. If specified, only dev resources attached to these nodes will be returned. If not specified, all dev resources in the file will be returned. |  |

### Return type

[**models::InlineObject36**](inline_object_36.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## post_dev_resources

> models::InlineObject37 post_dev_resources(post_dev_resources_request)
Create dev resources

Bulk create dev resources across multiple files. Dev resources that are successfully created will show up in the links_created array in the response.  If there are any dev resources that cannot be created, you may still get a 200 response. These resources will show up in the errors array. Some reasons a dev resource cannot be created include:  - Resource points to a `file_key` that cannot be found. - The node already has the maximum of 10 dev resources. - Another dev resource for the node has the same url.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**post_dev_resources_request** | [**PostDevResourcesRequest**](PostDevResourcesRequest.md) | A list of dev resources that you want to create. | [required] |

### Return type

[**models::InlineObject37**](inline_object_37.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## put_dev_resources

> models::InlineObject38 put_dev_resources(put_dev_resources_request)
Update dev resources

Bulk update dev resources across multiple files.  Ids for dev resources that are successfully updated will show up in the `links_updated` array in the response.  If there are any dev resources that cannot be updated, you may still get a 200 response. These resources will show up in the `errors` array.

### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**put_dev_resources_request** | [**PutDevResourcesRequest**](PutDevResourcesRequest.md) | A list of dev resources that you want to update. | [required] |

### Return type

[**models::InlineObject38**](inline_object_38.md)

### Authorization

[OAuth2](../README.md#OAuth2), [PersonalAccessToken](../README.md#PersonalAccessToken), [PlanAccessToken](../README.md#PlanAccessToken)

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

